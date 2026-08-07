---
name: preview-collaboration-connect
description: Connect the current agent conversation to an existing preview by Preview ID.
---

Connect the current agent conversation to one existing preview and enter its
continuous feedback workflow. This skill accepts exactly one Preview ID.

Use only the shared `collab` CLI. The preview must already exist, and the agent
conversation must already be open in the same project workspace.

**Prerequisites**: this skill requires the `collab` CLI. When a `collab`
command fails with `command not found`, report that the CLI is not installed,
point at `cargo install --path . --locked` from an html-agent-collab checkout,
and stop without substituting another preview mechanism.

Read `collab <command> --help` when an argument is unclear. Do not run a
`collab` command that this skill does not name.

Use the fixed lifecycle terms consistently: Open preview creates the runtime,
Connect agent creates a new attachment, Resume continues a paused attachment,
Stop collaboration deactivates an attachment while preserving the preview, and
Close preview shuts down the runtime.

## Validate the invocation

Require one and only one argument before executing any CLI command.

- With no argument, report the usage error
  `$preview-collaboration-connect <preview-id>` and stop.
- With more than one argument, report the same usage error and stop. Do not
  choose an argument.
- Treat the Preview ID as a non-secret selector. Do not ask for or copy a
  project path, port, PID, control token, feedback item, or attachment ID.

## Attach to the selected preview

1. Retain the single argument as `<preview-id>`.
2. Resolve `<current-workspace>` from the agent conversation's current project
   workspace. This value is passed as `--project` to the CLI, which walks up
   ancestor directories from it to find the session file; a directory above
   the session's `projectRoot` will not find it. Discovery is limited to that
   workspace and its canonical ancestors; do not inspect sibling projects, the
   user home directory, global registries, network services, or cloud state.
3. Run
   `collab attach --project <current-workspace> --session <preview-id> --agent <claude-code|codex>`.
4. Continue only when attach succeeds. Retain `data.previewSessionId` and the
   new `data.attachment.attachmentId`; use the attachmentId in every wait and
   feedback mutation, and pass the previewSessionId with `--session` whenever
   explicit session selection is needed.

Every successful invocation creates a new attachment. Never reactivate or
reuse an inactive attachment based on agent kind, process ID, TUI session ID,
or a previous conversation.

If attach returns `preview-not-running`, `preview-session-mismatch`,
`attachment-capacity`, an unreachable response, or another transport failure,
surface the structured error and stop. Do not create a replacement preview,
select another session, search globally, or enter the wait loop. A failed
attach does not enter the feedback wait loop.

## Continuous feedback loop

1. **Wait** — Run
   `collab wait --project <current-workspace> --session <preview-session-id> --attachment <attachment-id> --json`.
2. **Acknowledge** — For a `feedback` event, run
   `collab feedback set-state --project <current-workspace> --session <preview-session-id> <feedback-id> acknowledged --expected pending --attachment <attachment-id>`.
3. **Inspect** — Run
   `collab feedback show --project <current-workspace> --session <preview-session-id> <feedback-id>`.
   Read the text, kind, DOM elements, viewport, orphaned flag, attachments, and
   editable vector payload before deciding what to change. For
   `preview-draft`, also read `previewDraft.selector`, `beforeHtml`, and
   `afterHtml`. Treat `beforeHtml` and `afterHtml` as complete documents and
   the selector and element metadata only as a focus hint.
4. **Mark working** — Run
   `collab feedback set-state --project <current-workspace> --session <preview-session-id> <feedback-id> working --expected acknowledged --attachment <attachment-id>`.
5. **Modify** — Make only the requested project changes. Preserve unrelated
   user edits and use the supplied DOM or painting context as the target. For
   `preview-draft`, use the selector, element metadata, `beforeHtml`, and
   `afterHtml` to compare the complete documents and make the smallest source
   change that produces the approved rendered result. If the current source
   differs from `beforeHtml` and cannot be merged safely, or the source target
   is absent or ambiguous, do not guess; leave source unchanged and finish the
   item as `failed` with a specific reason.
6. **Verify** — Confirm the existing preview reloads, inspect relevant page
   state with `collab eval`, and use `collab screenshot` when visual proof is
   relevant.
7. **Finish lifecycle** —
   - On success, set the item to `resolved` with expected state `working` and
     the retained attachment ID.
   - If verification fails, set the item to `failed` with expected state
     `working`, the retained attachment ID, and the verification reason.
8. **Wait again** — Return to step 1 without attaching again.

Continue until wait returns `collaboration.stop`, the user explicitly ends
collaboration, or an unrecoverable error prevents safe work.

## Pause and resume

When the user requests Pause, retain the known attachment ID and run
`collab pause --project <current-workspace> --session <preview-session-id> --attachment <attachment-id>`.

- `paused` means no feedback lease exists; the blocking wait remains available
  and continues heartbeat while paused.
- `pause-requested` means finish only the current feedback item through its
  terminal transition before the attachment becomes paused.
- Do not attach again and do not busy-poll while paused.

When the user requests Resume, run
`collab resume --project <current-workspace> --session <preview-session-id> --attachment <attachment-id>`.
Resume preserves the same attachment ID and blocking wait.

## Persisted terminal state is the completion boundary

After a transition to `resolved` or `failed`, require the response to confirm
that terminal state was persisted. Until terminal confirmation is complete:

- Do not report the feedback as complete.
- Do not return from the feedback cycle.
- Do not issue another wait for the next feedback item.

If Stop collaboration occurs before terminal confirmation, stop without
claiming completion. The non-terminal item remains eligible for lease recovery.

## Ambiguous terminal response

If the terminal transition has a connection reset, timeout, or transport
error:

1. Read the feedback item again with `collab feedback show`.
2. If it is `resolved` or `failed`, terminal confirmation is complete.
3. If it is `working` and this attachment still owns the lease, retry the
   original terminal transition.
4. If it is `pending`, inactive, missing its lease, or owned by another
   attachment, report lost ownership and do not issue another wait.
5. If read-back fails, stop the feedback loop without reporting completion.

SIGINT interrupts a blocking wait without stopping collaboration or changing
the attachment identity. Retain the attachment ID so Stop collaboration can
target it. Do not treat SIGINT as preview shutdown.
