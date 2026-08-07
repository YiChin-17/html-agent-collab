---
name: preview-collaboration-start
description: Open or reuse one HTML preview in foreground manual or background collaboration mode.
---

Open preview for a single HTML entry. The skill accepts an optional
`foreground` or `background` mode.

Use only the shared `collab` CLI. Do not call the loopback HTTP service
directly and do not require MCP configuration.

**Prerequisites**: this skill requires the `collab` CLI. When a `collab`
command fails with `command not found`, report that the CLI is not installed,
point at `cargo install --path . --locked` from an html-agent-collab checkout,
and stop without substituting another preview mechanism.

Read `collab <command> --help` when an argument is unclear. Do not run a
`collab` command that this skill does not name.

## Session discovery — `--project`

Every `collab` subcommand except `open` accepts `--project <dir>` to locate
the active preview session. The CLI starts at that directory and walks **up**
ancestor directories to find a session file; it defaults to `.`.

`collab open` anchors the session to the entry file's parent directory (the
`projectRoot` field in `--background` JSON output). Retain this path and pass
it as `--project` in all subsequent commands. A directory **above**
`projectRoot` will not find the session — discovery walks up, not down.

In foreground mode (no JSON output), derive `<project>` as the directory
containing the resolved `<entry>` file.

All `<project>` placeholders below refer to this retained or derived value.

## Choose the Open preview mode

Explicit mode takes precedence over intent inference.

- Choose foreground for an unambiguous request to inspect or manually operate
  the preview.
- Choose background for an unambiguous request to start collaboration, process
  feedback, verify, capture screenshots, or wait for feedback.
- If no mode is explicit and the request is not uniquely classified, ask the
  user to choose `foreground` or `background` before starting a preview or
  creating an attachment. Do not silently choose a mode.

## Foreground manual mode

1. Resolve the user's HTML file or directory as `<entry>`.
2. From the project root, run `collab open <entry>` in a host terminal that
   remains available for the preview process. Wait for the session to become
   healthy.
3. From a separate CLI invocation, run
   `collab attach --project <project> --agent <claude-code|codex>` so the
   collaboration overlay is usable.
4. Confirm the active preview and return to the user. Do not run `collab wait`
   in foreground mode and do not enter the feedback wait loop.

`detach` still detaches this attachment while preserving the preview; `close`
still shuts down the preview runtime.

## Background collaboration mode

Use this mode for the continuous feedback wait loop.

1. Resolve the user's HTML file or directory as `<entry>`.
2. From the project root, run `collab open <entry> --background`.
   Continue only after it returns `opened` or `reused`. A `reused` result for
   the same canonical entry preserves the existing session, process, port, and
   preview window.
3. Run
   `collab attach --project <project> --agent <claude-code|codex>`.
4. Retain `data.projectRoot` as `<project>` for all subsequent `--project`
   flags. Also retain `data.attachment.attachmentId` as the known attachment ID
   and `data.previewSessionId`; when a later command fails with
   `ambiguous-preview`, pass `--session <previewSessionId>` to select this
   preview. Use the attachment ID in every wait and feedback mutation. Do not
   attach again merely because a wait was interrupted.

## Continuous feedback loop

1. **Wait** — Run
   `collab wait --project <project> --attachment <attachment-id> --json`.
2. **Acknowledge** — For a `feedback` event, run
   `collab feedback set-state --project <project> <feedback-id> acknowledged --expected pending --attachment <attachment-id>`.
3. **Inspect** — Run
   `collab feedback show --project <project> <feedback-id>`.
   Read the text, kind, DOM elements, viewport, orphaned flag, attachments, and
   editable vector payload before deciding what to change. For
   `preview-draft`, also read `previewDraft.selector`, `beforeHtml`, and
   `afterHtml`. Treat `beforeHtml` and `afterHtml` as complete documents and
   the selector and element metadata only as a focus hint.
4. **Mark working** — Run
   `collab feedback set-state --project <project> <feedback-id> working --expected acknowledged --attachment <attachment-id>`.
5. **Modify** — Make only the requested project changes. Preserve unrelated
   user edits and use the supplied DOM or painting context as the target. For
   `preview-draft`, use the selector, element metadata, `beforeHtml`, and
   `afterHtml` to compare the complete documents and make the smallest source
   change that produces the approved rendered result. If the current source
   differs from `beforeHtml` and cannot be merged safely, or the source target
   is absent or ambiguous, do not guess; leave source unchanged and finish the
   item as `failed` with a specific reason.
6. **Verify** — Confirm the existing preview reloads, inspect relevant page
   state with `collab eval`, and capture `collab screenshot` when visual proof
   is relevant.
7. **Finish lifecycle** —
   - On success, run
     `collab feedback set-state --project <project> <feedback-id> resolved --expected working --attachment <attachment-id>`.
   - If verification fails, run
     `collab feedback set-state --project <project> <feedback-id> failed --expected working --attachment <attachment-id> --reason "<verification reason>"`.
     Report the evidence and do not mark that item resolved.
8. **Wait again** — Return to step 1 in the same invocation. Two sequential
   feedback items must not require another start invocation.

Continue until `collab wait` returns `collaboration.stop`, the user explicitly
ends collaboration, or an unrecoverable error prevents safe work.

## Pause and resume control

When the user requests Pause, retain the known attachment ID and run:

`collab pause --project <project> --attachment <attachment-id>`

- A `paused` result means no current feedback lease exists. The current or next
  `collab wait` must remain blocked and continues heartbeat while paused.
- A `pause-requested` result means the attachment may finish only its current
  feedback item. Complete the normal terminal transition to `resolved` or
  `failed`; the attachment becomes paused before another item can be leased.
- While paused, do not report the workflow as stopped and do not attach again.
  Do not busy-poll status or wait. Leave the blocking wait in progress.

When the user requests Resume, run:

`collab resume --project <project> --attachment <attachment-id>`

Resume preserves the same attachment ID, preview session, and conversation.
The existing blocking wait continues until feedback or `collaboration.stop`
arrives; resume does not require a replacement attachment or a model-visible
resume event.

## Persisted terminal state is the completion boundary

After step 7 (Finish lifecycle), check the `feedback set-state` response. If it
returns the item persisted as `resolved` or `failed`, terminal confirmation is
complete. Until terminal confirmation is complete:

- Do not report the feedback as complete or successfully processed.
- Do not return from this feedback cycle or detach as complete.
- Do not issue another `wait` for the next feedback item.

If the user explicitly stops collaboration before terminal confirmation, stop
without claiming that feedback was completed. The non-terminal item remains
eligible for lease recovery.

## Ambiguous terminal response

If `feedback set-state` for a terminal transition does not return a clear
success or failure response (connection reset, timeout, or transport error):

1. Run `collab feedback show --project <project> <feedback-id>` to read back
   the persisted record.
2. If the record is `resolved` or `failed`: terminal confirmation is complete;
   proceed normally.
3. If the record is `working` and the current attachment holds an active lease:
   retry the original terminal transition.
4. If the record is `pending`, the attachment is inactive, the lease is missing,
   or the lease owner does not match: stop claiming completion and report lost
   ownership. Do not issue another `wait`.
5. If the read-back itself fails: stop the current feedback loop. Do not report
   the item as complete.

## SIGINT and restart semantics

The user may need to interrupt a blocking wait before invoking
`preview-collaboration-stop`. SIGINT, including exit status 130 with an
`interrupted` event, preserves the attachment and feedback state. In the same
conversation, retain the known attachment ID so the stop workflow can detach
that exact attachment. Do not interpret SIGINT as preview shutdown.

If the user later starts collaboration for the same canonical entry, run the
normal background open and attach sequence again. The open result should be
`reused`; the new attachment resumes the loop in the existing preview window.

## Failure handling

- On `invalid-entry`, `entry-conflict`, or `preview-start-timeout`, report the
  structured error and do not attach.
- On `state-conflict`, inspect the current feedback before deciding whether
  ownership was lost or the work is already complete.
- If the attachment is inactive or its lease is absent, do not mutate the
  feedback. Report the error and stop this loop.
- On `collaboration.stop`, finish successfully without issuing another wait.
