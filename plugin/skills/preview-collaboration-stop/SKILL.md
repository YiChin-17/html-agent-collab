---
name: preview-collaboration-stop
description: Detach collaboration from an HTML preview while keeping the preview runtime open.
---

Stop collaboration without closing the preview.

Use only `collab detach`. Never use `collab close` in this workflow.

**Prerequisites**: this skill requires the `collab` CLI. When a `collab`
command fails with `command not found`, report that the CLI is not installed,
point at `cargo install --path . --locked` from an html-agent-collab checkout,
and stop.

Pause and Stop are distinct. Pause preserves the attachment so the same wait
can resume; Stop makes it inactive. `collab detach` accepts an attachment that
is active, pause-requested, or paused and wakes its wait with
`collaboration.stop`.

## Session discovery — `--project`

`--project <dir>` locates the preview session by walking **up** ancestor
directories from `<dir>` (defaults to `.`). Pass the `projectRoot` retained
from `collab open` or `collab attach`, or the directory containing the entry
HTML file. A directory above `projectRoot` will not find the session.

## Same conversation

When start and stop run in the same conversation, use the known attachment ID
retained by `preview-collaboration-start`:

`collab detach --project <project> --attachment <attachment-id>`

This targets only that collaboration attachment. A blocking or subsequent wait
returns `collaboration.stop`; the preview runtime, window, watcher, port,
session ID, and session file remain available.

## Another conversation

When stop runs in another conversation and no known attachment ID is available,
run:

`collab detach --project <project>`

The CLI applies these selection rules:

- With zero active attachments, success status is `already-detached`.
- With one active attachment, that attachment is detached.
- With multiple active attachments, the command fails with
  `ambiguous-attachment` and candidate IDs. Surface the ambiguity and do not
  choose an attachment for the user.

For selection, pause-requested and paused attachments remain connected and are
included with active attachments. A detached inactive attachment is not a
candidate.

Stop does not open a missing preview and does not modify project files.
