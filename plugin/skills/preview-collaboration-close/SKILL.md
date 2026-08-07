---
name: preview-collaboration-close
description: Close an HTML preview runtime and stop all collaboration attachments.
---

Close the preview runtime when the user is finished with the preview itself.

**Prerequisites**: this skill requires the `collab` CLI. When a `collab`
command fails with `command not found`, report that the CLI is not installed,
point at `cargo install --path . --locked` from an html-agent-collab checkout,
and stop.

## Session discovery — `--project`

`--project <dir>` locates the preview session by walking **up** ancestor
directories from `<dir>` (defaults to `.`). Pass the `projectRoot` retained
from `collab open` or `collab attach`, or the directory containing the entry
HTML file. A directory above `projectRoot` will not find the session.

## Close

Run:

`collab close --project <project>`

This wakes active waiters with `collaboration.stop`, shuts down the server and
preview window, stops the watcher, and removes the active session file. It is
valid for an active or already-detached preview.

If no preview exists, report the structured `preview-not-running` error. Do not
modify project files.
