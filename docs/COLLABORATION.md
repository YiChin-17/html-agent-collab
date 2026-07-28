# Collaboration guide

## Start modes

Explicit `foreground` or `background` mode takes precedence. Without one, requests to inspect or manually operate the preview use foreground; requests to collaborate, process feedback, verify, capture screenshots, or wait use background. If intent is ambiguous, the skill asks which mode to use.

| Mode | Choose when | Result |
| --- | --- | --- |
| `foreground` | Inspect or manually operate the page | Keeps the process in an available host terminal, waits for health, attaches the agent, and returns without entering the feedback wait loop |
| `background` | Collaborate, process feedback, verify, capture screenshots, or wait | Opens or reuses a launcher-independent preview, attaches the agent, and continuously processes later feedback |

Manual foreground mode does not enter the feedback wait loop. Background collaboration mode keeps one continuous feedback loop. The single start skill handles both modes.

```text
$preview-collaboration-start path/to/page.html foreground
$preview-collaboration-start path/to/page.html background
```

One background invocation handles later feedback; it does not need to be invoked once per item.

## Collaboration lifecycle

The native collaboration dashboard displays `No agent connected` when no attachment is connected. Select Connect agent to reveal the current Preview ID, the exact connect command, and Copy command. Paste that command into an agent conversation already open in the same project workspace:

```text
$preview-collaboration-connect <preview-id>
```

The Preview ID is a non-secret selector; the local CLI reads authorization details from the workspace session registry. It is not a credential and the copied command contains no project path, port, PID, control token, feedback content, or attachment ID. Every successful Connect agent workflow creates a new attachment without reactivating a stopped attachment or creating another preview.

With multiple attachments, select one before using Pause, Resume, or Stop collaboration. A paused attachment shows Resume for that same attachment rather than Connect agent.

| Action | Attachment result | Preview result |
| --- | --- | --- |
| Connect agent | Creates a new attachment for an existing Preview ID | Reuses the open preview without opening another runtime |
| Pause | Becomes `paused`, or `pause-requested` until current feedback reaches `resolved` or `failed`; keeps the same attachment and blocking wait | Remains open; new feedback is rejected when no other active attachment exists |
| Resume | Reactivates the same attachment and wait | Reuses the same preview |
| Stop collaboration | Makes the selected attachment inactive and interrupts its wait | Keeps the server, watcher, session file, and WKWebView open; hides the toolbar after the last detach |
| Close preview | Stops every attachment after native confirmation | Closes the preview runtime and removes the active session file |

### Pause collaboration after current feedback

```text
collab pause --project <project> [--attachment <id>]
collab resume --project <project> [--attachment <id>]
```

### Stop collaboration while keeping the preview

```text
$preview-collaboration-stop
```

If the host TUI is blocked in `collab wait`, press Ctrl+C before invoking stop in the same conversation. SIGINT interrupts the wait but does not remove attachment identity or close the preview.

### Close the preview runtime

```text
$preview-collaboration-close
```

Open preview through the start skill when a runtime does not exist. Starting the same canonical entry again returns `reused` and preserves the session ID, port, PID, and single WKWebView. After Stop collaboration, use Connect agent to create a new attachment and restart continuous collaboration in that existing preview. The toolbar becomes visible after attach and stays hidden while detached.

## Feedback types and marker states

Overlay submission is accepted only after stale attachment expiry leaves at least one active attachment. A paused-only preview returns `collaboration-paused`; zero connected, all inactive, or all stale attachments return `collaboration-inactive`. Rejected submissions do not publish JSON, SVG, or PNG artifacts.

| Type | Context recorded | Marker behavior |
| --- | --- | --- |
| Element comment | Selected DOM element and page context | Creates a page marker |
| Painting | Freehand, rectangle, arrow, or text-label marks plus SVG and native PNG | Appears in dashboard status; no element marker |
| Textbox | Free-form note and page context | Appears in dashboard status; no element marker |
| Preview Draft | Before/after complete HTML documents plus one element's selector and metadata as a focus hint | Uses the existing pending-to-terminal lifecycle and selected element marker |

## Preview Draft

Choose Draft in the native dashboard to expand the existing app window. The
current Preview remains on the left in the same single WKWebView; the right Draft pane
contains the complete HTML source and its own Undo, Redo, Reset,
and Apply to source compact toolbar. The toolbar uses AppKit small controls in a
34-point strip; an inline validation message adds a 24-point strip only while
the message is visible. Those four actions do not appear in the Preview titlebar.
Selecting one rendered element on the left uses its `outerHTML` only to focus a
unique exact range without replacing the complete document.

After the first layout, the divider starts at Preview 60 percent and Draft 40
percent. You can drag it while keeping Preview at least 640 points wide and
Draft at least 360 points wide. Closing and reopening Draft resets the divider
to 60:40 subject to those minimums; the manually selected ratio is not saved.

Preview Draft is HTML-only and edits the in-memory rendered DOM. It does not write project files
or provide source line mapping. The editor uses a monospaced font
and basic HTML syntax colors for tag names and delimiters, attribute names,
quoted values, comments, and doctype declarations. Embedded CSS and JavaScript
remain editable as plain text but use the base editor color. Apply to source
submits one pending `preview-draft` feedback item. Its `beforeHtml` and `afterHtml` are complete
HTML documents; the selector and element metadata are a focus hint. The agent
compares the two documents and makes the smallest unambiguous source change.
If the current source differs from `beforeHtml` and cannot be merged safely,
or no unique source target exists, the agent leaves the source unchanged and
marks the item failed with a specific reason.

Reload, navigation, and preview close discard the visual draft. A dynamic
framework may rerender the selected element before Apply; in that case the
next operation reports a missing target instead of guessing another element.

| Marker state | Visible result |
| --- | --- |
| `pending` / `acknowledged` | Marker remains visible |
| `working` | A working marker remains visible with its current state |
| `resolved` | The resolved feedback removes its marker without reloading |
| `failed` | A failed marker remains visible with its failure reason |

Reload reconciliation restores the newest persisted marker state. Detaching the last attachment closes unsubmitted editors, clears draft painting marks, and hides the toolbar, highlights, and painting layer without changing submitted feedback, user HTML, or the rendered page. Agents verify modifications with `collab eval` and use `collab screenshot` when visual evidence is needed.
