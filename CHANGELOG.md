# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.3.0] - 2026-07-28

### Added

- Preview Draft editing with complete-document agent handoff and rendered-element focus hints.

### Changed

- Preview Draft now uses a native split workspace with basic HTML syntax highlighting, compact controls, a draggable 60:40 initial split, and bounded pane widths.

### Fixed

- Preview close now releases all server leases so the runtime exits cleanly.
- Raw-text HTML highlighting now validates closing tags before leaving `script` or `style` content.

## [0.2.1] - 2026-07-23

### Added

- Agent attachment pairing links each collaboration attachment to a specific agent session.

### Changed

- Dashboard toolbar uses a popover-based info display instead of inline labels.
- State label styled as a button with state-based bezel color.

### Fixed

- Correct `setOrientation` API usage and retain popover objects to prevent premature deallocation.

## [0.1.2] - 2026-07-21

### Fixed

- Background previews remain available after the short-lived launcher exits, so later CLI invocations can reuse and control the same native runtime.

### Changed

- Preview start workflows now distinguish foreground manual operation from background collaboration and prompt when the intended mode is ambiguous.

## [0.1.1] - 2026-07-20

### Changed

- The preview collaboration toolbar can be dragged away from page content and remains within the viewport after mode or window size changes.

## [0.1.0] - 2026-07-20

### Added

- MIT OR Apache-2.0 dual licensing and open-source contribution policies.
- Source-only GitHub release automation and repository quality gates.

### Changed

- Public documentation now reflects the Rust, Tauri 2, and WKWebView implementation.
