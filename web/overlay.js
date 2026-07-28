// collab feedback overlay：以 WKUserScript 注入，不修改使用者 HTML。
// spec「Isolated collaboration overlay」：Shadow DOM 隔離、每次 navigation 恰一份、
// 不使用原生阻斷式對話框。
// spec「Painting and textbox feedback」：freehand/rectangle/arrow/text label 繪於
// 固定定位 SVG layer，送出時持久化 PNG 與可編輯 vector payload。
(function () {
  "use strict";

  // duplicate-injection guard：同一 JS context 只安裝一次（reload 會建立新 context）。
  if (window.__collabOverlayInstalled) return;
  window.__collabOverlayInstalled = true;

  var CSS_TEXT = __OVERLAY_CSS_JSON__;
  var HOST_TAG = "collab-overlay-host";
  var DRAFT_KEY = "__collab_draft__";
  var FEEDBACK_ENDPOINT = "/__collab__/overlay/feedback";
  var DRAFT_STATE_ENDPOINT = "/__collab__/overlay/draft-state";
  var STATUS_ENDPOINT = "/__collab__/status";
  var SVG_NS = "http://www.w3.org/2000/svg";
  var HISTORY_LIMIT = 50;
  var PREVIEW_DRAFT_HISTORY_LIMIT = 50;
  var PREVIEW_DRAFT_DOCUMENT_LIMIT = 262144;
  var PAINT_TOOLS = [
    { id: "select", label: "Select" },
    { id: "freehand", label: "Freehand" },
    { id: "rect", label: "Rectangle" },
    { id: "arrow", label: "Arrow" },
    { id: "label", label: "Text label" },
    { id: "eraser", label: "Eraser" }
  ];
  var PAINT_ACTIONS = [
    { id: "undo", label: "Undo" },
    { id: "redo", label: "Redo" },
    { id: "clear", label: "Clear all" }
  ];

  var ui = {};
  var state = {
    mode: null,          // null | "comment" | "paint" | "draft"
    tool: "freehand",    // paint 子工具：select | freehand | rect | arrow | label | eraser
    hovered: null,
    selected: null,
    editorKind: null,    // "element-comment" | "textbox" | "painting"
    marks: [],
    draft: null,          // 進行中的 mark
    selectedMark: null,
    paintGesture: null,
    historyUndo: [],
    historyRedo: [],
    labelPosition: null,
    active: false,
    dragPointerId: null,
    dragOffsetX: 0,
    dragOffsetY: 0,
    editorDragPointerId: null,
    editorDragOffsetX: 0,
    editorDragOffsetY: 0,
    lastMarkerRevision: -1
  };
  var previewDraftManager = {
    status: "idle",
    pageUrl: null,
    target: null,
    focusHtml: null,
    focusMetadata: null,
    originalHtml: null,
    currentHtml: null,
    selector: null,
    undoHistory: [],
    redoHistory: [],
    error: null,
    submissionPromise: null
  };

  // ---------- element metadata ----------

  function cssPath(el) {
    if (el.id) return "#" + CSS.escape(el.id);
    var parts = [];
    var node = el;
    while (node && node.nodeType === 1 && node !== document.documentElement) {
      var part = node.tagName.toLowerCase();
      if (node.id) {
        parts.unshift(part + "#" + CSS.escape(node.id));
        return parts.join(" > ");
      }
      var parent = node.parentElement;
      if (parent) {
        var sameTag = Array.prototype.filter.call(parent.children, function (c) {
          return c.tagName === node.tagName;
        });
        if (sameTag.length > 1) {
          part += ":nth-of-type(" + (sameTag.indexOf(node) + 1) + ")";
        }
      }
      parts.unshift(part);
      node = parent;
    }
    parts.unshift("html");
    return parts.join(" > ");
  }

  function domPath(el) {
    var path = [];
    var node = el;
    while (node && node.nodeType === 1) {
      var parent = node.parentElement;
      var index = parent ? Array.prototype.indexOf.call(parent.children, node) : 0;
      path.unshift({ tag: node.tagName.toLowerCase(), index: index });
      node = parent;
    }
    return path;
  }

  function elementPayload(el) {
    var rect = el.getBoundingClientRect();
    var attributes = {};
    for (var i = 0; i < el.attributes.length; i++) {
      attributes[el.attributes[i].name] = el.attributes[i].value;
    }
    return {
      tag: el.tagName.toLowerCase(),
      selector: cssPath(el),
      domPath: domPath(el),
      attributes: attributes,
      rect: { x: rect.x, y: rect.y, width: rect.width, height: rect.height },
      textSnippet: (el.textContent || "").trim().slice(0, 120)
    };
  }

  function viewportPayload() {
    return {
      width: window.innerWidth,
      height: window.innerHeight,
      scrollX: window.scrollX,
      scrollY: window.scrollY,
      devicePixelRatio: window.devicePixelRatio
    };
  }

  function feedbackBody(kind, text, elements) {
    return {
      kind: kind,
      text: text,
      pageUrl: location.href,
      viewport: viewportPayload(),
      elements: elements
    };
  }

  function submitFeedback(body) {
    return fetch(FEEDBACK_ENDPOINT, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body)
    }).then(function (response) {
      return response.json().then(function (json) {
        if (!response.ok) {
          var error = new Error(json.message || "feedback rejected");
          error.code = json.code || "submission-failed";
          throw error;
        }
        return json;
      });
    });
  }

  // ---------- Preview Draft ----------

  function previewDraftState() {
    return {
      status: previewDraftManager.status,
      pageUrl: previewDraftManager.pageUrl,
      selector: previewDraftManager.selector,
      focusHtml: previewDraftManager.focusHtml,
      originalHtml: previewDraftManager.originalHtml,
      currentHtml: previewDraftManager.currentHtml,
      dirty: previewDraftManager.originalHtml !== null &&
        previewDraftManager.currentHtml !== previewDraftManager.originalHtml,
      undoDepth: previewDraftManager.undoHistory.length,
      redoDepth: previewDraftManager.redoHistory.length,
      error: previewDraftManager.error ? {
        code: previewDraftManager.error.code,
        message: previewDraftManager.error.message
      } : null
    };
  }

  function publishPreviewDraftState() {
    fetch(DRAFT_STATE_ENDPOINT, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(previewDraftState())
    }).catch(function () {
      // Native panel may be closed or the preview may be shutting down.
    });
  }

  function showPreviewDraftError(code, message) {
    previewDraftManager.error = { code: code, message: message };
    publishPreviewDraftState();
    return previewDraftState();
  }

  function clearPreviewDraftError() {
    previewDraftManager.error = null;
    publishPreviewDraftState();
  }

  function invalidPreviewDraftTarget(element) {
    if (!(element instanceof Element)) return true;
    var root = element.getRootNode();
    return element.tagName.toLowerCase() === HOST_TAG ||
      (root instanceof ShadowRoot && root.host &&
        root.host.tagName.toLowerCase() === HOST_TAG);
  }

  function openPreviewDraftFor(element) {
    if (invalidPreviewDraftTarget(element)) {
      return showPreviewDraftError("invalid-target", "Preview Draft requires a rendered page element");
    }
    if (!element.isConnected) {
      return showPreviewDraftError("target-missing", "The selected Preview Draft target is no longer present");
    }
    if (previewDraftManager.status === "idle" ||
        typeof previewDraftManager.currentHtml !== "string") {
      return showPreviewDraftError(
        "draft-source-missing",
        "Load the complete Preview Draft source before selecting an element"
      );
    }
    if (previewDraftManager.target === element) {
      clearPreviewDraftError();
      return previewDraftState();
    }

    var html = element.outerHTML;
    var firstMatch = previewDraftManager.currentHtml.indexOf(html);
    var lastMatch = previewDraftManager.currentHtml.lastIndexOf(html);
    previewDraftManager.target = element;
    previewDraftManager.selector = cssPath(element);
    previewDraftManager.focusHtml = html;
    previewDraftManager.focusMetadata = elementPayload(element);
    if (firstMatch < 0 || firstMatch !== lastMatch) {
      return showPreviewDraftError(
        "source-location-unavailable",
        "The rendered element does not have one unique exact match in the current source"
      );
    }
    clearPreviewDraftError();
    return previewDraftState();
  }

  function parsePreviewDraftDocument(html) {
    if (typeof html !== "string" ||
        new TextEncoder().encode(html).length > PREVIEW_DRAFT_DOCUMENT_LIMIT) {
      return {
        error: {
          code: typeof html === "string" ? "document-too-large" : "invalid-document",
          message: typeof html === "string" ?
            "The complete HTML document exceeds the Preview Draft size limit" :
            "Preview Draft requires a complete HTML document string"
        }
      };
    }
    if (!html.trim() ||
        !/<html(?:\s|>)/i.test(html) ||
        !/<head(?:\s|>)/i.test(html) ||
        !/<body(?:\s|>)/i.test(html)) {
      return {
        error: {
          code: "invalid-document",
          message: "Preview Draft requires html, head, and body structure"
        }
      };
    }

    var parsed;
    try {
      parsed = new DOMParser().parseFromString(html, "text/html");
    } catch (_error) {
      return {
        error: {
          code: "invalid-document",
          message: "Preview Draft could not parse the complete HTML document"
        }
      };
    }
    if (!parsed.documentElement || !parsed.head || !parsed.body) {
      return {
        error: {
          code: "invalid-document",
          message: "Preview Draft requires html, head, and body structure"
        }
      };
    }
    return { document: parsed };
  }

  function copyPreviewDraftAttributes(target, source) {
    Array.prototype.slice.call(target.attributes).forEach(function (attribute) {
      target.removeAttribute(attribute.name);
    });
    Array.prototype.slice.call(source.attributes).forEach(function (attribute) {
      target.setAttribute(attribute.name, attribute.value);
    });
  }

  function renderPreviewDraftDocument(html) {
    var parsed = parsePreviewDraftDocument(html);
    if (parsed.error) return parsed;
    var host = document.querySelector(HOST_TAG);
    copyPreviewDraftAttributes(document.documentElement, parsed.document.documentElement);
    copyPreviewDraftAttributes(document.head, parsed.document.head);
    copyPreviewDraftAttributes(document.body, parsed.document.body);
    document.head.innerHTML = parsed.document.head.innerHTML;
    document.body.innerHTML = parsed.document.body.innerHTML;
    if (host) document.body.appendChild(host);
    previewDraftManager.currentHtml = html;
    previewDraftManager.target = null;
    if (previewDraftManager.selector) {
      try {
        previewDraftManager.target = document.querySelector(previewDraftManager.selector);
      } catch (_error) {
        previewDraftManager.target = null;
      }
    }
    return { html: previewDraftManager.currentHtml };
  }

  function loadPreviewDraftSource(input) {
    if (!input || typeof input.pageUrl !== "string" || !input.pageUrl ||
        typeof input.html !== "string") {
      return showPreviewDraftError(
        "invalid-document",
        "Preview Draft requires a page URL and complete HTML document"
      );
    }
    if (previewDraftState().dirty && previewDraftManager.pageUrl !== input.pageUrl) {
      return showPreviewDraftError(
        "draft-conflict",
        "Apply, Reset, or continue editing the current Preview Draft document"
      );
    }
    var parsed = parsePreviewDraftDocument(input.html);
    if (parsed.error) {
      return showPreviewDraftError(parsed.error.code, parsed.error.message);
    }
    previewDraftManager.status = "editing";
    previewDraftManager.pageUrl = input.pageUrl;
    previewDraftManager.target = null;
    previewDraftManager.focusHtml = null;
    previewDraftManager.focusMetadata = null;
    previewDraftManager.selector = null;
    previewDraftManager.originalHtml = input.html;
    previewDraftManager.currentHtml = input.html;
    previewDraftManager.undoHistory = [];
    previewDraftManager.redoHistory = [];
    clearPreviewDraftError();
    return previewDraftState();
  }

  function pushPreviewDraftHistory(history, html) {
    history.push(html);
    if (history.length > PREVIEW_DRAFT_HISTORY_LIMIT) {
      history.splice(0, history.length - PREVIEW_DRAFT_HISTORY_LIMIT);
    }
  }

  function applyPreviewDraft(input) {
    if (!input || typeof input.html !== "string") {
      return showPreviewDraftError(
        "invalid-document",
        "Preview Draft requires a complete HTML document"
      );
    }
    if (previewDraftManager.status === "idle") {
      return showPreviewDraftError("draft-source-missing", "Load Preview Draft source first");
    }
    if (previewDraftManager.status === "submitted") {
      return showPreviewDraftError("draft-submitted", "The Preview Draft was already submitted");
    }
    if (input.html === previewDraftManager.currentHtml) {
      clearPreviewDraftError();
      return previewDraftState();
    }

    var previousHtml = previewDraftManager.currentHtml;
    var replaced = renderPreviewDraftDocument(input.html);
    if (replaced.error) {
      return showPreviewDraftError(replaced.error.code, replaced.error.message);
    }
    pushPreviewDraftHistory(previewDraftManager.undoHistory, previousHtml);
    previewDraftManager.redoHistory = [];
    clearPreviewDraftError();
    return previewDraftState();
  }

  function undoPreviewDraft() {
    if (!previewDraftManager.undoHistory.length) return previewDraftState();
    var previousHtml = previewDraftManager.undoHistory[
      previewDraftManager.undoHistory.length - 1
    ];
    var currentHtml = previewDraftManager.currentHtml;
    var replaced = renderPreviewDraftDocument(previousHtml);
    if (replaced.error) {
      return showPreviewDraftError(replaced.error.code, replaced.error.message);
    }
    previewDraftManager.undoHistory.pop();
    pushPreviewDraftHistory(previewDraftManager.redoHistory, currentHtml);
    clearPreviewDraftError();
    return previewDraftState();
  }

  function redoPreviewDraft() {
    if (!previewDraftManager.redoHistory.length) return previewDraftState();
    var nextHtml = previewDraftManager.redoHistory[
      previewDraftManager.redoHistory.length - 1
    ];
    var currentHtml = previewDraftManager.currentHtml;
    var replaced = renderPreviewDraftDocument(nextHtml);
    if (replaced.error) {
      return showPreviewDraftError(replaced.error.code, replaced.error.message);
    }
    previewDraftManager.redoHistory.pop();
    pushPreviewDraftHistory(previewDraftManager.undoHistory, currentHtml);
    clearPreviewDraftError();
    return previewDraftState();
  }

  function resetPreviewDraft() {
    if (previewDraftManager.status === "idle") return previewDraftState();
    if (previewDraftManager.status === "submitted") {
      return showPreviewDraftError("draft-submitted", "The Preview Draft was already submitted");
    }
    var replaced = renderPreviewDraftDocument(previewDraftManager.originalHtml);
    if (replaced.error) {
      return showPreviewDraftError(replaced.error.code, replaced.error.message);
    }
    previewDraftManager.status = "editing";
    previewDraftManager.undoHistory = [];
    previewDraftManager.redoHistory = [];
    clearPreviewDraftError();
    if (ui.highlight) hideHighlight();
    return previewDraftState();
  }

  function submitPreviewDraft() {
    var stateBeforeSubmit = previewDraftState();
    if (!previewDraftManager.selector || !previewDraftManager.focusMetadata) {
      return Promise.resolve(
        showPreviewDraftError("target-missing", "Select one Preview Draft target before applying")
      );
    }
    if (previewDraftManager.status === "submitted") {
      return Promise.resolve(
        showPreviewDraftError("draft-submitted", "The Preview Draft was already submitted")
      );
    }
    if (!stateBeforeSubmit.dirty) {
      return Promise.resolve(
        showPreviewDraftError("draft-not-dirty", "Apply to source requires a modified Preview Draft")
      );
    }
    if (previewDraftManager.submissionPromise) {
      return previewDraftManager.submissionPromise;
    }

    var metadata = previewDraftManager.target && previewDraftManager.target.isConnected ?
      elementPayload(previewDraftManager.target) :
      previewDraftManager.focusMetadata;
    metadata.selector = previewDraftManager.selector;
    var body = feedbackBody("preview-draft", "Apply Preview Draft", [metadata]);
    body.previewDraft = {
      selector: previewDraftManager.selector,
      beforeHtml: previewDraftManager.originalHtml,
      afterHtml: previewDraftManager.currentHtml
    };
    previewDraftManager.submissionPromise = submitFeedback(body).then(
      function (result) {
        previewDraftManager.status = "submitted";
        previewDraftManager.submissionPromise = null;
        clearPreviewDraftError();
        setMode(null);
        var submitted = previewDraftState();
        submitted.feedbackId = result.id;
        submitted.feedbackState = "pending";
        return submitted;
      },
      function (error) {
        previewDraftManager.submissionPromise = null;
        return showPreviewDraftError(
          error.code || "submission-failed",
          error.message || "Preview Draft submission failed"
        );
      }
    );
    return previewDraftManager.submissionPromise;
  }

  // ---------- painting marks ----------

  function markBBox(mark) {
    if (mark.type === "freehand") {
      var xs = mark.points.map(function (p) { return p[0]; });
      var ys = mark.points.map(function (p) { return p[1]; });
      var minX = Math.min.apply(null, xs);
      var minY = Math.min.apply(null, ys);
      return { x: minX, y: minY, width: Math.max.apply(null, xs) - minX, height: Math.max.apply(null, ys) - minY };
    }
    if (mark.type === "rect") {
      return { x: mark.x, y: mark.y, width: mark.width, height: mark.height };
    }
    if (mark.type === "arrow") {
      var x = Math.min(mark.x1, mark.x2);
      var y = Math.min(mark.y1, mark.y2);
      return { x: x, y: y, width: Math.abs(mark.x2 - mark.x1), height: Math.abs(mark.y2 - mark.y1) };
    }
    // label：以文字位置附近的小區域計 overlap。
    return { x: mark.x, y: mark.y - 14, width: Math.max(40, (mark.text || "").length * 7), height: 18 };
  }

  function intersectionArea(a, b) {
    var x = Math.max(a.x, b.x);
    var y = Math.max(a.y, b.y);
    var right = Math.min(a.x + a.width, b.x + b.width);
    var bottom = Math.min(a.y + a.height, b.y + b.height);
    if (right <= x || bottom <= y) return 0;
    return (right - x) * (bottom - y);
  }

  // spec「Painting crosses multiple elements」：每個 element 的 overlap ratio =
  // intersection / element area（跨 marks 取最大值），依 ratio 降冪排序。
  function computeOverlaps(marks) {
    var boxes = marks.map(markBBox).filter(function (b) { return b.width > 0 && b.height > 0; });
    if (!boxes.length) return [];
    var results = [];
    var all = document.body.querySelectorAll("*");
    for (var i = 0; i < all.length; i++) {
      var el = all[i];
      var tag = el.tagName.toLowerCase();
      if (tag === HOST_TAG || tag === "script" || tag === "style") continue;
      var rect = el.getBoundingClientRect();
      var area = rect.width * rect.height;
      if (area <= 0) continue;
      var best = 0;
      for (var j = 0; j < boxes.length; j++) {
        var ratio = intersectionArea(
          { x: rect.x, y: rect.y, width: rect.width, height: rect.height },
          boxes[j]
        ) / area;
        if (ratio > best) best = ratio;
      }
      if (best >= 0.02) {
        var payload = elementPayload(el);
        payload.overlapRatio = Math.round(best * 1000) / 1000;
        results.push(payload);
      }
    }
    results.sort(function (a, b) { return b.overlapRatio - a.overlapRatio; });
    return results.slice(0, 10);
  }

  function renderMark(mark) {
    var node;
    if (mark.type === "freehand") {
      node = document.createElementNS(SVG_NS, "path");
      node.setAttribute(
        "d",
        mark.points
          .map(function (p, i) { return (i === 0 ? "M" : "L") + p[0] + " " + p[1]; })
          .join(" ")
      );
      node.setAttribute("fill", "none");
    } else if (mark.type === "rect") {
      node = document.createElementNS(SVG_NS, "rect");
      node.setAttribute("x", mark.x);
      node.setAttribute("y", mark.y);
      node.setAttribute("width", mark.width);
      node.setAttribute("height", mark.height);
      node.setAttribute("fill", "none");
    } else if (mark.type === "arrow") {
      node = document.createElementNS(SVG_NS, "line");
      node.setAttribute("x1", mark.x1);
      node.setAttribute("y1", mark.y1);
      node.setAttribute("x2", mark.x2);
      node.setAttribute("y2", mark.y2);
      node.setAttribute("marker-end", "url(#collab-arrowhead)");
    } else if (mark.type === "label") {
      node = document.createElementNS(SVG_NS, "text");
      node.setAttribute("x", mark.x);
      node.setAttribute("y", mark.y);
      node.textContent = mark.text || "";
      node.setAttribute("fill", "#e33");
      node.setAttribute("font-size", "14");
      node.setAttribute("stroke", "none");
      ui.paintLayer.appendChild(node);
      return;
    }
    node.setAttribute("stroke", "#e33");
    node.setAttribute("stroke-width", "2.5");
    ui.paintLayer.appendChild(node);
  }

  function renderMarks() {
    while (ui.paintLayer.childNodes.length > 1) {
      ui.paintLayer.removeChild(ui.paintLayer.lastChild);
    }
    state.marks.forEach(renderMark);
    if (state.draft) renderMark(state.draft);
    refreshSelectionIndicator();
  }

  function cloneMarks(marks) {
    return JSON.parse(JSON.stringify(marks));
  }

  function commitMarksMutation(snapshot) {
    state.historyUndo.push(cloneMarks(snapshot || state.marks));
    if (state.historyUndo.length > HISTORY_LIMIT) {
      state.historyUndo.splice(0, state.historyUndo.length - HISTORY_LIMIT);
    }
    state.historyRedo = [];
  }

  function restoreMarks(snapshot) {
    state.marks = cloneMarks(snapshot);
    state.draft = null;
    state.selectedMark = null;
    renderMarks();
    refreshToolbar();
    saveDraft();
  }

  function undoMarks() {
    if (!state.historyUndo.length) return;
    state.historyRedo.push(cloneMarks(state.marks));
    restoreMarks(state.historyUndo.pop());
  }

  function redoMarks() {
    if (!state.historyRedo.length) return;
    state.historyUndo.push(cloneMarks(state.marks));
    restoreMarks(state.historyRedo.pop());
  }

  function clearPaintSelection() {
    state.selectedMark = null;
    state.paintGesture = null;
    refreshSelectionIndicator();
  }

  function addMark(mark) {
    commitMarksMutation();
    state.marks.push(mark);
    renderMarks();
    refreshToolbar();
    saveDraft();
  }

  function clearAllMarks() {
    if (!state.marks.length) return;
    commitMarksMutation();
    state.marks = [];
    clearPaintSelection();
    renderMarks();
    refreshToolbar();
    saveDraft();
  }

  function clearMarks() {
    state.marks = [];
    state.draft = null;
    state.historyUndo = [];
    state.historyRedo = [];
    state.labelPosition = null;
    cancelLabelInput();
    clearPaintSelection();
    renderMarks();
  }

  function pointDistanceToSegment(x, y, x1, y1, x2, y2) {
    var dx = x2 - x1;
    var dy = y2 - y1;
    if (dx === 0 && dy === 0) return Math.hypot(x - x1, y - y1);
    var t = ((x - x1) * dx + (y - y1) * dy) / (dx * dx + dy * dy);
    t = Math.max(0, Math.min(1, t));
    return Math.hypot(x - (x1 + t * dx), y - (y1 + t * dy));
  }

  function markHit(mark, x, y) {
    var tolerance = 8;
    if (mark.type === "freehand") {
      for (var i = 1; i < mark.points.length; i++) {
        if (pointDistanceToSegment(x, y, mark.points[i - 1][0], mark.points[i - 1][1], mark.points[i][0], mark.points[i][1]) <= tolerance) return true;
      }
      return mark.points.length === 1 && Math.hypot(x - mark.points[0][0], y - mark.points[0][1]) <= tolerance;
    }
    if (mark.type === "arrow") {
      return pointDistanceToSegment(x, y, mark.x1, mark.y1, mark.x2, mark.y2) <= tolerance;
    }
    if (mark.type === "rect") {
      return x >= mark.x - tolerance && x <= mark.x + mark.width + tolerance &&
        y >= mark.y - tolerance && y <= mark.y + mark.height + tolerance;
    }
    var box = markBBox(mark);
    return x >= box.x - tolerance && x <= box.x + box.width + tolerance &&
      y >= box.y - tolerance && y <= box.y + box.height + tolerance;
  }

  function hitMarkIndex(x, y) {
    for (var i = state.marks.length - 1; i >= 0; i--) {
      if (markHit(state.marks[i], x, y)) return i;
    }
    return -1;
  }

  function translateMark(mark, dx, dy) {
    var moved = cloneMarks([mark])[0];
    if (moved.type === "freehand") {
      moved.points = moved.points.map(function (point) { return [point[0] + dx, point[1] + dy]; });
    } else if (moved.type === "rect" || moved.type === "label") {
      moved.x += dx;
      moved.y += dy;
    } else if (moved.type === "arrow") {
      moved.x1 += dx;
      moved.y1 += dy;
      moved.x2 += dx;
      moved.y2 += dy;
    }
    return moved;
  }

  function removeSelectedMark() {
    if (state.selectedMark === null || !state.marks[state.selectedMark]) return;
    commitMarksMutation();
    state.marks.splice(state.selectedMark, 1);
    clearPaintSelection();
    renderMarks();
    refreshToolbar();
    saveDraft();
  }

  function serializeSvg() {
    return new XMLSerializer().serializeToString(ui.paintLayer);
  }

  // ---------- UI ----------

  function toolbarButton(label, onClick) {
    var button = document.createElement("button");
    button.textContent = label;
    button.addEventListener("click", onClick);
    ui.toolbar.appendChild(button);
    return button;
  }

  function setSvgAttributes(node, attributes) {
    Object.keys(attributes).forEach(function (name) {
      node.setAttribute(name, attributes[name]);
    });
    return node;
  }

  function paintToolIcon(tool) {
    var icon = document.createElementNS(SVG_NS, "svg");
    icon.setAttribute("viewBox", "0 0 24 24");
    icon.setAttribute("aria-hidden", "true");
    icon.setAttribute("focusable", "false");

    if (tool.id === "select") {
      icon.appendChild(setSvgAttributes(document.createElementNS(SVG_NS, "path"), {
        d: "m5 3 14 9-6 1 3 7-3 1-3-7-5 4z",
        fill: "none"
      }));
    } else if (tool.id === "freehand") {
      icon.appendChild(setSvgAttributes(document.createElementNS(SVG_NS, "path"), {
        d: "M3 17c3-8 5 2 8-5s4 7 10-5",
        fill: "none"
      }));
    } else if (tool.id === "rect") {
      icon.appendChild(setSvgAttributes(document.createElementNS(SVG_NS, "rect"), {
        x: "4",
        y: "5",
        width: "16",
        height: "14",
        rx: "1",
        fill: "none"
      }));
    } else if (tool.id === "arrow") {
      icon.appendChild(setSvgAttributes(document.createElementNS(SVG_NS, "path"), {
        d: "M4 18 19 6m-7 0h7v7",
        fill: "none"
      }));
    } else if (tool.id === "label") {
      icon.appendChild(setSvgAttributes(document.createElementNS(SVG_NS, "path"), {
        d: "M5 5h14M12 5v14M8 19h8",
        fill: "none"
      }));
    } else if (tool.id === "eraser") {
      icon.appendChild(setSvgAttributes(document.createElementNS(SVG_NS, "path"), {
        d: "m7 18 10-10 4 4-10 10H7zM4 21h16",
        fill: "none"
      }));
    } else if (tool.id === "undo") {
      icon.appendChild(setSvgAttributes(document.createElementNS(SVG_NS, "path"), {
        d: "M9 7 4 12l5 5M5 12h9a5 5 0 1 1 0 10",
        fill: "none"
      }));
    } else if (tool.id === "redo") {
      icon.appendChild(setSvgAttributes(document.createElementNS(SVG_NS, "path"), {
        d: "m15 7 5 5-5 5m4-5h-9a5 5 0 1 0 0 10",
        fill: "none"
      }));
    } else {
      icon.appendChild(setSvgAttributes(document.createElementNS(SVG_NS, "path"), {
        d: "M5 7h14m-9 3v7m4-7v7M9 7l1-3h4l1 3M7 21h10",
        fill: "none"
      }));
    }
    return icon;
  }

  function paintToolButton(tool, onClick) {
    var button = document.createElement("button");
    button.className = "collab-tool";
    button.setAttribute("aria-label", tool.label);
    button.dataset.tooltip = tool.label;
    var icon = paintToolIcon(tool);
    button.appendChild(icon);
    button.addEventListener("focus", function () {
      button.classList.add("collab-focused");
    });
    button.addEventListener("blur", function () {
      button.classList.remove("collab-focused");
    });
    button.addEventListener("click", onClick);
    ui.toolbar.appendChild(button);
    return button;
  }

  function buildUi(shadow) {
    var style = document.createElement("style");
    style.textContent = CSS_TEXT;
    shadow.appendChild(style);

    ui.toolbar = document.createElement("div");
    ui.toolbar.className = "collab-toolbar";
    ui.dragHandle = document.createElement("button");
    ui.dragHandle.className = "collab-drag-handle";
    ui.dragHandle.setAttribute("type", "button");
    ui.dragHandle.setAttribute("aria-label", "Move collaboration toolbar");
    ui.dragHandle.title = "Move collaboration toolbar";
    ui.toolbar.appendChild(ui.dragHandle);
    ui.commentButton = toolbarButton("Element", function () {
      setMode(state.mode === "comment" ? null : "comment");
    });
    ui.paintButton = toolbarButton("Paint", function () {
      setMode(state.mode === "paint" ? null : "paint");
    });
    ui.toolButtons = {};
    PAINT_TOOLS.forEach(function (tool) {
      ui.toolButtons[tool.id] = paintToolButton(tool, function () {
        state.tool = tool.id;
        clearPaintSelection();
        refreshToolbar();
      });
    });
    ui.actionButtons = {};
    PAINT_ACTIONS.forEach(function (action) {
      ui.actionButtons[action.id] = paintToolButton(action, function () {
        if (action.id === "undo") undoMarks();
        else if (action.id === "redo") redoMarks();
        else clearAllMarks();
      });
    });
    ui.sendPaintButton = toolbarButton("Send paint", function () {
      openEditor("painting", null);
    });
    ui.noteButton = toolbarButton("Note", function () {
      openEditor("textbox", null);
    });
    shadow.appendChild(ui.toolbar);

    ui.highlight = document.createElement("div");
    ui.highlight.className = "collab-highlight";
    shadow.appendChild(ui.highlight);

    // 固定定位 SVG painting layer；marks 在 overlay 內，WebKit snapshot 一起擷取。
    ui.paintLayer = document.createElementNS(SVG_NS, "svg");
    ui.paintLayer.setAttribute("class", "collab-paint-layer");
    ui.paintLayer.setAttribute("xmlns", SVG_NS);
    ui.paintLayer.style.cssText =
      "position:fixed;left:0;top:0;width:100vw;height:100vh;pointer-events:none;z-index:4;";
    var defs = document.createElementNS(SVG_NS, "defs");
    var marker = document.createElementNS(SVG_NS, "marker");
    marker.setAttribute("id", "collab-arrowhead");
    marker.setAttribute("markerWidth", "8");
    marker.setAttribute("markerHeight", "8");
    marker.setAttribute("refX", "6");
    marker.setAttribute("refY", "3");
    marker.setAttribute("orient", "auto");
    var arrowPath = document.createElementNS(SVG_NS, "path");
    arrowPath.setAttribute("d", "M0,0 L6,3 L0,6 z");
    arrowPath.setAttribute("fill", "#e33");
    marker.appendChild(arrowPath);
    defs.appendChild(marker);
    ui.paintLayer.appendChild(defs);
    shadow.appendChild(ui.paintLayer);

    ui.selectionIndicator = document.createElement("div");
    ui.selectionIndicator.className = "collab-selection-indicator";
    shadow.appendChild(ui.selectionIndicator);

    ui.labelInput = document.createElement("input");
    ui.labelInput.className = "collab-label-input";
    ui.labelInput.setAttribute("type", "text");
    ui.labelInput.setAttribute("aria-label", "Text label");
    ui.labelInput.setAttribute("placeholder", "Label text");
    ui.labelInput.addEventListener("keydown", function (event) {
      if (event.key === "Enter") {
        event.preventDefault();
        commitLabelInput();
      } else if (event.key === "Escape") {
        event.preventDefault();
        cancelLabelInput();
      }
    });
    ui.labelInput.addEventListener("blur", cancelLabelInput);
    shadow.appendChild(ui.labelInput);

    ui.editor = document.createElement("div");
    ui.editor.className = "collab-editor";
    ui.editorDragHandle = document.createElement("button");
    ui.editorDragHandle.className = "collab-editor-drag-handle";
    ui.editorDragHandle.setAttribute("type", "button");
    ui.editorDragHandle.setAttribute("aria-label", "Move feedback editor");
    ui.editorDragHandle.title = "Move feedback editor";
    ui.editorTarget = document.createElement("div");
    ui.editorTarget.className = "collab-editor-target";
    ui.textarea = document.createElement("textarea");
    ui.textarea.placeholder = "Describe the change you want…";
    ui.textarea.addEventListener("input", saveDraft);
    ui.hint = document.createElement("div");
    ui.hint.className = "collab-editor-hint";
    ui.hint.textContent = "Enter feedback text before submitting.";
    var actions = document.createElement("div");
    actions.className = "collab-editor-actions";
    ui.cancelButton = document.createElement("button");
    ui.cancelButton.className = "collab-cancel";
    ui.cancelButton.textContent = "Cancel";
    ui.cancelButton.addEventListener("click", closeEditor);
    ui.submitButton = document.createElement("button");
    ui.submitButton.className = "collab-submit";
    ui.submitButton.textContent = "Send";
    ui.submitButton.addEventListener("click", attemptSubmit);
    actions.appendChild(ui.cancelButton);
    actions.appendChild(ui.submitButton);
    ui.editor.appendChild(ui.editorDragHandle);
    ui.editor.appendChild(ui.editorTarget);
    ui.editor.appendChild(ui.textarea);
    ui.editor.appendChild(ui.hint);
    ui.editor.appendChild(actions);
    shadow.appendChild(ui.editor);

    ui.toast = document.createElement("div");
    ui.toast.className = "collab-toast";
    shadow.appendChild(ui.toast);

    ui.markerLayer = document.createElement("div");
    ui.markerLayer.className = "collab-marker-layer";
    shadow.appendChild(ui.markerLayer);

    refreshToolbar();
  }

  function toast(message) {
    ui.toast.textContent = message;
    ui.toast.classList.add("visible");
    setTimeout(function () {
      ui.toast.classList.remove("visible");
    }, 2200);
  }

  function clampToolbarPosition(left, top) {
    var rect = ui.toolbar.getBoundingClientRect();
    var maxLeft = Math.max(0, window.innerWidth - rect.width);
    var maxTop = Math.max(0, window.innerHeight - rect.height);
    return {
      left: Math.min(Math.max(0, left), maxLeft),
      top: Math.min(Math.max(0, top), maxTop)
    };
  }

  function setToolbarPosition(left, top) {
    var position = clampToolbarPosition(left, top);
    ui.toolbar.style.left = position.left + "px";
    ui.toolbar.style.top = position.top + "px";
    ui.toolbar.style.right = "auto";
  }

  function clampCurrentToolbarPosition() {
    if (!ui.toolbar || !ui.toolbar.style.left) return;
    setToolbarPosition(
      parseFloat(ui.toolbar.style.left),
      parseFloat(ui.toolbar.style.top)
    );
  }

  function refreshSelectionIndicator() {
    if (state.selectedMark === null || !state.marks[state.selectedMark]) {
      ui.selectionIndicator.style.display = "none";
      return;
    }
    var box = markBBox(state.marks[state.selectedMark]);
    ui.selectionIndicator.style.display = "block";
    ui.selectionIndicator.style.left = box.x - 4 + "px";
    ui.selectionIndicator.style.top = box.y - 4 + "px";
    ui.selectionIndicator.style.width = Math.max(8, box.width + 8) + "px";
    ui.selectionIndicator.style.height = Math.max(8, box.height + 8) + "px";
  }

  function openLabelInput(x, y) {
    state.labelPosition = { x: x, y: y };
    ui.labelInput.value = "";
    ui.labelInput.style.display = "block";
    var width = ui.labelInput.offsetWidth || 180;
    var height = ui.labelInput.offsetHeight || 28;
    ui.labelInput.style.left = Math.min(Math.max(0, x), Math.max(0, window.innerWidth - width)) + "px";
    ui.labelInput.style.top = Math.min(Math.max(0, y), Math.max(0, window.innerHeight - height)) + "px";
    ui.labelInput.focus();
  }

  function commitLabelInput() {
    var text = ui.labelInput.value.trim();
    if (!text || !state.labelPosition) return;
    var position = state.labelPosition;
    cancelLabelInput();
    addMark({ type: "label", x: position.x, y: position.y, text: text });
  }

  function cancelLabelInput() {
    if (!ui.labelInput) return;
    state.labelPosition = null;
    ui.labelInput.value = "";
    ui.labelInput.style.display = "none";
  }

  function refreshToolbar() {
    ui.toolbar.style.display = state.active ? "flex" : "none";
    ui.commentButton.classList.toggle("active", state.mode === "comment");
    ui.paintButton.classList.toggle("active", state.mode === "paint");
    Object.keys(ui.toolButtons).forEach(function (tool) {
      var button = ui.toolButtons[tool];
      button.style.display = state.mode === "paint" ? "" : "none";
      button.classList.toggle("active", state.mode === "paint" && state.tool === tool);
    });
    Object.keys(ui.actionButtons).forEach(function (action) {
      var button = ui.actionButtons[action];
      button.style.display = state.mode === "paint" ? "" : "none";
      button.disabled = action === "undo" ? !state.historyUndo.length :
        action === "redo" ? !state.historyRedo.length : !state.marks.length;
    });
    ui.sendPaintButton.style.display =
      state.mode === "paint" || state.marks.length ? "" : "none";
    ui.paintLayer.style.pointerEvents = state.mode === "paint" ? "auto" : "none";
    ui.paintLayer.style.cursor = state.mode === "paint" ? "crosshair" : "";
    ui.paintLayer.style.display = state.active ? "" : "none";
    if (state.active) clampCurrentToolbarPosition();
  }

  function setActive(active) {
    state.active = !!active;
    if (!ui.toolbar) return;
    if (!state.active) {
      closeEditor();
      clearMarks();
      state.mode = null;
      hideHighlight();
    }
    refreshToolbar();
  }

  function setMode(mode) {
    if (mode === "draft" && editorOpen()) closeEditor();
    state.mode = mode;
    if (mode !== "comment" && mode !== "draft") hideHighlight();
    if (mode !== "paint") {
      state.draft = null;
      state.paintGesture = null;
      cancelLabelInput();
      clearPaintSelection();
    }
    if (mode === "draft") {
      hideHighlight();
      previewDraftManager.error = null;
    }
    refreshToolbar();
  }

  function hideHighlight() {
    state.hovered = null;
    ui.highlight.style.display = "none";
    ui.highlight.classList.remove("collab-draft-highlight");
  }

  function moveHighlight(el) {
    if (!el) return;
    var rect = el.getBoundingClientRect();
    ui.highlight.style.display = "block";
    ui.highlight.classList.toggle("collab-draft-highlight", state.mode === "draft");
    ui.highlight.style.left = rect.left + "px";
    ui.highlight.style.top = rect.top + "px";
    ui.highlight.style.width = rect.width + "px";
    ui.highlight.style.height = rect.height + "px";
  }

  function pickableElement(event) {
    var el = document.elementFromPoint(event.clientX, event.clientY);
    if (!el || el.tagName.toLowerCase() === HOST_TAG || el === document.documentElement) {
      return null;
    }
    return el;
  }

  // ---------- editor ----------

  function clampEditorPosition(left, top) {
    var rect = ui.editor.getBoundingClientRect();
    var maxLeft = Math.max(0, window.innerWidth - rect.width);
    var maxTop = Math.max(0, window.innerHeight - rect.height);
    return {
      left: Math.min(Math.max(0, left), maxLeft),
      top: Math.min(Math.max(0, top), maxTop)
    };
  }

  function setEditorPosition(left, top) {
    var position = clampEditorPosition(left, top);
    ui.editor.style.left = position.left + "px";
    ui.editor.style.top = position.top + "px";
  }

  function clampCurrentEditorPosition() {
    if (!editorOpen() || !ui.editor.style.left) return;
    setEditorPosition(parseFloat(ui.editor.style.left), parseFloat(ui.editor.style.top));
  }

  function openEditor(kind, el) {
    state.editorKind = kind;
    state.selected = el;
    if (el) {
      ui.editorTarget.textContent = cssPath(el);
      var rect = el.getBoundingClientRect();
      ui.editor.style.left = rect.left + "px";
      ui.editor.style.top = rect.bottom + 8 + "px";
    } else {
      ui.editorTarget.textContent =
        kind === "painting" ? "Painting feedback (" + state.marks.length + " marks)" : "Note to agent";
      ui.editor.style.left = window.innerWidth / 2 - 170 + "px";
      ui.editor.style.top = "80px";
    }
    ui.editor.style.display = "block";
    clampCurrentEditorPosition();
    ui.hint.style.display = "none";
    ui.textarea.focus();
    saveDraft();
  }

  function openEditorFor(el) {
    openEditor("element-comment", el);
  }

  function closeEditor() {
    state.selected = null;
    state.editorKind = null;
    ui.editor.style.display = "none";
    state.editorDragPointerId = null;
    ui.textarea.value = "";
    ui.hint.style.display = "none";
    try {
      sessionStorage.removeItem(DRAFT_KEY);
    } catch (e) {}
  }

  function editorOpen() {
    return ui.editor.style.display === "block";
  }

  // spec「Submit empty text without visual markup」：保持編輯器開啟、不建立 feedback。
  function attemptSubmit() {
    var kind = state.editorKind;
    var text = ui.textarea.value.trim();
    if (kind === "element-comment") {
      if (!state.selected) return Promise.resolve(null);
      if (!text) {
        ui.hint.style.display = "block";
        return Promise.resolve(null);
      }
      return sendAndClose(feedbackBody(kind, text, [elementPayload(state.selected)]));
    }
    if (kind === "textbox") {
      if (!text) {
        ui.hint.style.display = "block";
        return Promise.resolve(null);
      }
      return sendAndClose(feedbackBody(kind, text, []));
    }
    if (kind === "painting") {
      if (!state.marks.length && !text) {
        ui.hint.style.display = "block";
        return Promise.resolve(null);
      }
      return submitPainting(text);
    }
    return Promise.resolve(null);
  }

  function sendAndClose(body) {
    return submitFeedback(body).then(
      function (result) {
        closeEditor();
        setMode(null);
        toast("Feedback sent");
        return result;
      },
      function (error) {
        toast("Failed: " + error.message);
        return null;
      }
    );
  }

  // spec「Submit painting feedback」：PNG + editable vector payload + overlap 排序。
  function submitPainting(text) {
    clearPaintSelection();
    var body = feedbackBody("painting", text || "", computeOverlaps(state.marks));
    body.marks = state.marks;
    body.svg = serializeSvg();
    return submitFeedback(body).then(
      function (result) {
        clearMarks();
        closeEditor();
        setMode(null);
        toast("Painting sent");
        return result;
      },
      function (error) {
        toast("Failed: " + error.message);
        return null;
      }
    );
  }

  // ---------- draft persistence（reload 後恢復未送出內容） ----------

  function saveDraft() {
    try {
      sessionStorage.setItem(
        DRAFT_KEY,
        JSON.stringify({
          kind: state.editorKind,
          selector: state.selected ? cssPath(state.selected) : null,
          text: ui.textarea.value,
          marks: state.marks
        })
      );
    } catch (e) {}
  }

  function restoreDraft() {
    var raw = null;
    try {
      raw = sessionStorage.getItem(DRAFT_KEY);
    } catch (e) {}
    if (!raw) return;
    try {
      var draft = JSON.parse(raw);
      if (draft.marks && draft.marks.length) {
        state.marks = draft.marks;
        renderMarks();
        refreshToolbar();
      }
      if (draft.kind === "element-comment" && draft.selector) {
        var el = document.querySelector(draft.selector);
        if (el) {
          openEditorFor(el);
          ui.textarea.value = draft.text || "";
        }
      } else if (draft.kind) {
        openEditor(draft.kind, null);
        ui.textarea.value = draft.text || "";
      }
    } catch (e) {}
  }

  // ---------- reload reconciliation ----------
  // spec「Reload reconciliation」：selector 解析 + tag 與 identity attributes
  // 全符合才重接 marker；否則設 orphaned，不得錯綁其他元素。

  function identityMatches(stored, el) {
    if (!stored || stored.tag !== el.tagName.toLowerCase()) return false;
    var attrs = stored.attributes || {};
    var keys = Object.keys(attrs);
    for (var i = 0; i < keys.length; i++) {
      var key = keys[i];
      if (key === "id" || key === "name" || key.indexOf("data-") === 0) {
        if (el.getAttribute(key) !== attrs[key]) return false;
      }
    }
    return true;
  }

  function resolveTarget(stored) {
    if (!stored || !stored.selector) return null;
    var el = null;
    try {
      el = document.querySelector(stored.selector);
    } catch (e) {
      return null;
    }
    if (!el || el.tagName.toLowerCase() === HOST_TAG) return null;
    return identityMatches(stored, el) ? el : null;
  }

  function addMarkerFor(el, item) {
    var rect = el.getBoundingClientRect();
    var marker = document.createElement("div");
    marker.className = "collab-marker collab-marker-" + item.state;
    marker.textContent = "!";
    var reason = item.failureReason ? ": " + item.failureReason : "";
    marker.title = item.text + reason;
    marker.setAttribute("role", "img");
    marker.setAttribute("aria-label", "Feedback " + item.state + reason);
    marker.dataset.feedbackId = item.id;
    marker.style.left = rect.right - 8 + "px";
    marker.style.top = rect.top - 8 + "px";
    ui.markerLayer.appendChild(marker);
  }

  function clearMarkers() {
    while (ui.markerLayer.firstChild) {
      ui.markerLayer.removeChild(ui.markerLayer.firstChild);
    }
  }

  function applyMarkerSnapshot(snapshot) {
    if (!snapshot || typeof snapshot.revision !== "number") return false;
    if (snapshot.revision < state.lastMarkerRevision) return false;
    state.lastMarkerRevision = snapshot.revision;
    clearMarkers();
    (snapshot.items || []).forEach(function (item) {
      if (item.state === "resolved") return;
      var el = resolveTarget(item.element);
      if (el) addMarkerFor(el, item);
    });
    return true;
  }

  // Explicit state class contracts: collab-marker-working, collab-marker-failed.
  // Failed descriptions include the persisted reason, for example verification failed.

  function postReconcile(id, orphaned) {
    return fetch(FEEDBACK_ENDPOINT + "/reconcile", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ id: id, orphaned: orphaned })
    });
  }

  function reconcile() {
    return fetch(FEEDBACK_ENDPOINT)
      .then(function (response) { return response.json(); })
      .then(function (payload) {
        var results = [];
        var updates = [];
        var markerItems = [];
        (payload.items || []).forEach(function (item) {
          if (item.kind !== "element-comment") return;
          var stored = item.elements && item.elements[0];
          var el = resolveTarget(stored);
          var matched = !!el;
          markerItems.push({
            id: item.id,
            state: item.state,
            text: item.text,
            failureReason: item.failureReason || null,
            element: stored
          });
          results.push({ id: item.id, matched: matched });
          if (item.orphaned === matched) {
            // matched 但仍標 orphaned、或未 matched 但未標 → 需更新旗標。
            updates.push(postReconcile(item.id, !matched));
          }
        });
        applyMarkerSnapshot({ revision: payload.revision || 0, items: markerItems });
        return Promise.all(updates).then(function () { return results; });
      });
  }

  // ---------- events ----------

  function onToolbarPointerDown(event) {
    if (!event.isPrimary || event.button !== 0 || state.dragPointerId !== null) return;
    var rect = ui.toolbar.getBoundingClientRect();
    state.dragPointerId = event.pointerId;
    state.dragOffsetX = event.clientX - rect.left;
    state.dragOffsetY = event.clientY - rect.top;
    ui.dragHandle.classList.add("dragging");
    ui.dragHandle.setPointerCapture(event.pointerId);
    event.preventDefault();
  }

  function onToolbarPointerMove(event) {
    if (event.pointerId !== state.dragPointerId) return;
    setToolbarPosition(event.clientX - state.dragOffsetX, event.clientY - state.dragOffsetY);
    event.preventDefault();
  }

  function finishToolbarDrag(event) {
    if (event.pointerId !== state.dragPointerId) return;
    if (ui.dragHandle.hasPointerCapture(event.pointerId)) {
      ui.dragHandle.releasePointerCapture(event.pointerId);
    }
    state.dragPointerId = null;
    ui.dragHandle.classList.remove("dragging");
  }

  function onEditorPointerDown(event) {
    if (!event.isPrimary || event.button !== 0 || state.editorDragPointerId !== null) return;
    var rect = ui.editor.getBoundingClientRect();
    state.editorDragPointerId = event.pointerId;
    state.editorDragOffsetX = event.clientX - rect.left;
    state.editorDragOffsetY = event.clientY - rect.top;
    ui.editorDragHandle.classList.add("dragging");
    ui.editorDragHandle.setPointerCapture(event.pointerId);
    event.preventDefault();
  }

  function onEditorPointerMove(event) {
    if (event.pointerId !== state.editorDragPointerId) return;
    setEditorPosition(event.clientX - state.editorDragOffsetX, event.clientY - state.editorDragOffsetY);
    event.preventDefault();
  }

  function finishEditorDrag(event) {
    if (event.pointerId !== state.editorDragPointerId) return;
    if (ui.editorDragHandle.hasPointerCapture(event.pointerId)) {
      ui.editorDragHandle.releasePointerCapture(event.pointerId);
    }
    state.editorDragPointerId = null;
    ui.editorDragHandle.classList.remove("dragging");
  }

  function editableControlFocused() {
    return document.activeElement === ui.textarea ||
      document.activeElement === ui.labelInput ||
      (ui.editor.getRootNode().activeElement === ui.textarea) ||
      (ui.labelInput.getRootNode().activeElement === ui.labelInput);
  }

  function onKeyDown(event) {
    if ((event.key !== "Delete" && event.key !== "Backspace") || editableControlFocused()) return;
    if (state.selectedMark === null) return;
    event.preventDefault();
    removeSelectedMark();
  }

  function onMouseMove(event) {
    if (!state.active ||
        (state.mode !== "comment" && state.mode !== "draft") ||
        editorOpen()) return;
    var el = pickableElement(event);
    if (!el) {
      hideHighlight();
      return;
    }
    state.hovered = el;
    moveHighlight(el);
  }

  function onClick(event) {
    if (!state.active ||
        (state.mode !== "comment" && state.mode !== "draft") ||
        editorOpen()) return;
    var el = pickableElement(event);
    if (!el) return;
    event.preventDefault();
    event.stopPropagation();
    if (state.mode === "draft") {
      var draftResult = openPreviewDraftFor(el);
      if (draftResult.error) return;
      state.hovered = previewDraftManager.target;
      moveHighlight(previewDraftManager.target);
      ui.highlight.classList.add("collab-draft-highlight");
      refreshToolbar();
      return;
    }
    hideHighlight();
    openEditorFor(el);
  }

  function onPaintDown(event) {
    if (!state.active || state.mode !== "paint" || editorOpen() || state.labelPosition) return;
    var x = event.clientX;
    var y = event.clientY;
    if (state.tool === "select") {
      var selectedIndex = hitMarkIndex(x, y);
      state.selectedMark = selectedIndex === -1 ? null : selectedIndex;
      if (selectedIndex !== -1) {
        state.paintGesture = {
          index: selectedIndex,
          startX: x,
          startY: y,
          before: cloneMarks(state.marks),
          original: cloneMarks([state.marks[selectedIndex]])[0]
        };
      }
      renderMarks();
      return;
    }
    if (state.tool === "eraser") {
      var erasedIndex = hitMarkIndex(x, y);
      if (erasedIndex === -1) return;
      commitMarksMutation();
      state.marks.splice(erasedIndex, 1);
      clearPaintSelection();
      renderMarks();
      refreshToolbar();
      saveDraft();
      return;
    }
    if (state.tool === "freehand") {
      state.draft = { type: "freehand", points: [[x, y]] };
    } else if (state.tool === "rect") {
      state.draft = { type: "rect", x: x, y: y, width: 0, height: 0, startX: x, startY: y };
    } else if (state.tool === "arrow") {
      state.draft = { type: "arrow", x1: x, y1: y, x2: x, y2: y };
    } else if (state.tool === "label") {
      openLabelInput(x, y);
      return;
    }
    renderMarks();
  }

  function onPaintMove(event) {
    if (state.paintGesture) {
      var gesture = state.paintGesture;
      state.marks[gesture.index] = translateMark(
        gesture.original,
        event.clientX - gesture.startX,
        event.clientY - gesture.startY
      );
      renderMarks();
      return;
    }
    if (!state.draft) return;
    var x = event.clientX;
    var y = event.clientY;
    if (state.draft.type === "freehand") {
      state.draft.points.push([x, y]);
    } else if (state.draft.type === "rect") {
      state.draft.x = Math.min(state.draft.startX, x);
      state.draft.y = Math.min(state.draft.startY, y);
      state.draft.width = Math.abs(x - state.draft.startX);
      state.draft.height = Math.abs(y - state.draft.startY);
    } else if (state.draft.type === "arrow") {
      state.draft.x2 = x;
      state.draft.y2 = y;
    }
    renderMarks();
  }

  function onPaintUp(event) {
    if (state.paintGesture) {
      var gesture = state.paintGesture;
      state.paintGesture = null;
      if (event.clientX !== gesture.startX || event.clientY !== gesture.startY) {
        commitMarksMutation(gesture.before);
        saveDraft();
        refreshToolbar();
      }
      renderMarks();
      return;
    }
    if (!state.draft) return;
    var mark = state.draft;
    state.draft = null;
    if (mark.type === "rect") {
      delete mark.startX;
      delete mark.startY;
    }
    addMark(mark);
  }

  // ---------- install ----------

  function install() {
    if (document.querySelector(HOST_TAG)) return;
    var host = document.createElement(HOST_TAG);
    // inline !important：抵抗針對 host tag 的 hostile page CSS。
    host.style.cssText =
      "position:fixed !important;left:0 !important;top:0 !important;" +
      "width:0 !important;height:0 !important;display:block !important;" +
      "visibility:visible !important;opacity:1 !important;" +
      "pointer-events:none !important;z-index:2147483647 !important;";
    var shadow = host.attachShadow({ mode: "open" });
    buildUi(shadow);
    (document.body || document.documentElement).appendChild(host);

    window.addEventListener("mousemove", onMouseMove, true);
    window.addEventListener("click", onClick, true);
    window.addEventListener("resize", clampCurrentToolbarPosition);
    window.addEventListener("resize", clampCurrentEditorPosition);
    window.addEventListener("keydown", onKeyDown, true);
    ui.dragHandle.addEventListener("pointerdown", onToolbarPointerDown);
    ui.dragHandle.addEventListener("pointermove", onToolbarPointerMove);
    ui.dragHandle.addEventListener("pointerup", finishToolbarDrag);
    ui.dragHandle.addEventListener("pointercancel", finishToolbarDrag);
    ui.editorDragHandle.addEventListener("pointerdown", onEditorPointerDown);
    ui.editorDragHandle.addEventListener("pointermove", onEditorPointerMove);
    ui.editorDragHandle.addEventListener("pointerup", finishEditorDrag);
    ui.editorDragHandle.addEventListener("pointercancel", finishEditorDrag);
    ui.paintLayer.addEventListener("mousedown", onPaintDown);
    ui.paintLayer.addEventListener("mousemove", onPaintMove);
    ui.paintLayer.addEventListener("mouseup", onPaintUp);

    setActive(state.active);
    publishPreviewDraftState();
    reconcile();
    fetch(STATUS_ENDPOINT)
      .then(function (response) { return response.json(); })
      .then(function (payload) {
        var active = payload.collaborationActive === true;
        setActive(active);
        if (active) restoreDraft();
      })
      .catch(function () {
        setActive(false);
      });
  }

  // agent/test 可用的程式化介面（eval contract 的一部分）。
  window.__collabOverlay = {
    version: 1,
    hostTag: HOST_TAG,
    setActive: setActive,
    isActive: function () {
      return state.active;
    },
    hostCount: function () {
      return document.querySelectorAll(HOST_TAG).length;
    },
    setMode: setMode,
    setTool: function (tool) {
      state.tool = tool;
      refreshToolbar();
    },
    openEditorFor: openEditorFor,
    openEditor: openEditor,
    attemptSubmit: attemptSubmit,
    editorOpen: editorOpen,
    loadPreviewDraftSource: loadPreviewDraftSource,
    openPreviewDraftFor: openPreviewDraftFor,
    applyPreviewDraft: applyPreviewDraft,
    previewDraftState: previewDraftState,
    undoPreviewDraft: undoPreviewDraft,
    redoPreviewDraft: redoPreviewDraft,
    resetPreviewDraft: resetPreviewDraft,
    submitPreviewDraft: submitPreviewDraft,
    buildElementPayload: elementPayload,
    submitElementComment: function (el, text) {
      return submitFeedback(feedbackBody("element-comment", text, [elementPayload(el)]));
    },
    submitTextbox: function (text) {
      return submitFeedback(feedbackBody("textbox", text, []));
    },
    addMark: addMark,
    clearMarks: clearMarks,
    marks: function () {
      return state.marks.slice();
    },
    computeOverlaps: function () {
      return computeOverlaps(state.marks);
    },
    submitPainting: submitPainting,
    reconcileNow: reconcile,
    syncFeedbackMarkers: applyMarkerSnapshot,
    markerCount: function () {
      return ui.markerLayer.childNodes.length;
    },
    markerFeedbackIds: function () {
      return Array.prototype.map.call(ui.markerLayer.childNodes, function (m) {
        return m.dataset.feedbackId;
      });
    }
  };

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", install, { once: true });
  } else {
    install();
  }
})();
