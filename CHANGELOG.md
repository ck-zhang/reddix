# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]
- Nothing yet.

## [0.2.7] - 2025-10-29
### Changed
- `ui.theme = default` now follows your terminal colors out of the box.

## [0.2.6] - 2025-10-29
### Fixed
- Escape now exits the fullscreen media preview instead of quitting the app.

## [0.2.5] - 2025-10-17
### Added
- First-run release notes view that opens the latest highlights directly in the content pane and keeps them available from the guided menu.

## [0.2.4] - 2025-10-16
### Added
- Commenting workflow inside the Comments pane so `w` lets you write threads and replies without leaving Reddix.

## [0.2.3] - 2025-10-15
### Added
- Toggle to hide or reveal NSFW posts mid-session without leaving the feed.
### Changed
- Status bar and help copy now highlight the NSFW mode so the current setting is obvious.

## [0.2.2] - 2025-10-14
### Fixed
- Respect Reddit's hidden comment scores.

## [0.2.1] - 2025-10-13
### Changed
- Inline previews now detect Kitty support at runtime, falling back to the legacy image placeholder while offering an external mpv launch when graphics are unavailable.
- Saving full-resolution media includes the original Reddit-hosted MP4 assets alongside images.
### Fixed
- Mouse capture is disabled on exit so terminals stop receiving stray pointer escape sequences after quitting.

## [0.2.0] - 2025-10-13
### Added
- Inline Kitty/mpv video playback for Reddit-hosted posts directly in the content pane, launched on demand from the actions menu with a configurable `REDDIX_MPV_PATH` override.
### Changed
- The actions menu now toggles inline video playback and reflects availability/status based on Kitty support and preview loading state.
- Inline previews reuse the same mpv session in loop mode so videos replay seamlessly without wiping the interface.

## [0.1.21] - 2025-10-10
### Fixed
- Retrieved the full subscription list by paging Reddit's API so the navigation pane shows every subreddit again.

## [0.1.20] - 2025-10-10
### Added
- Fullscreen media toggle (`f`) that refetches inline previews at terminal size and centers them on screen.

## [0.1.19] - 2025-10-10
### Changed
- Always emit Kitty inline media previews so supported terminals render images instantly while others simply show empty space.
- Made the Kitty probe opt-in via `REDDIX_EXPERIMENTAL_KITTY_PROBE` to dodge glitchy terminals.

## [0.1.18] - 2025-10-10
### Fixed
- Prevented inline media previews from overflowing on very narrow terminals and rescaled them automatically when the viewport changes.

## [0.1.17] - 2025-10-09
### Added
- `?` help overlay with grouped keybindings and contextual sections.
### Changed
- Footer now shows only essential shortcuts and highlights `h/l` pane focus hints.
- Help overlay copy now highlights general navigation heuristics (hjkl and Ctrl+H/J/K/L).

## [0.1.16] - 2025-10-08
### Added
- Clipboard shortcut (`y`) copies the highlighted comment to the system clipboard.

## [0.1.15] - 2025-10-08
### Added
- Comment sorting controls that mirror the post sort workflow, with a top-of-pane picker and `t` shortcut.

## [0.1.14] - 2025-10-07
### Added
- Command palette with fuzzy subreddit/user search and consolidated actions menu access.
- Full-resolution media saver (images only) that queues downloads without blocking the UI.
### Changed
- Navigation keys now respect typing mode so overlays no longer hijack text input.

## [0.1.13] - 2025-10-07
### Changed
- Wrapped subreddit sort shortcuts so the navigation pane stays readable on narrow terminals.

## [0.1.12] - 2025-10-07
### Changed
- Kept the selected subreddit centered by auto-scrolling the navigation list.

## [0.1.11] - 2025-10-06
### Added
- One-click updater that downloads and runs the latest installer from the banner.
### Changed
- Guided setup copy now highlights the example `config.yaml` and refreshed quick-start instructions.

## [0.1.10] - 2025-10-06
### Changed
- Simplified the guided authorization flow and trimmed conflicting shortcuts in the menu.
### Added
- Feature request tracker to capture community ideas in one place.

## [0.1.9] - 2025-10-06
### Fixed
- Restored `q` as the quit shortcut in the credentials form without blocking text entry.

## [0.1.8] - 2025-10-06
### Changed
- Resolved the remaining guided-menu shortcut clashes and refreshed the README preview image.

## [0.1.7] - 2025-10-06
### Fixed
- Cleared stale kitty previews when scrolling so inline media no longer leaves artifacts.

## [0.1.6] - 2025-10-06
### Changed
- Polished the update banner with clearer messaging, better selection defaults, and smoother post focus.

## [0.1.5] - 2025-10-05
### Added
- `--version`/`--help` flags with tests plus environment overrides to simulate update scenarios.
### Changed
- Kept the update banner visible even when the post list recenters.

## [0.1.4] - 2025-10-05
### Added
- Asynchronous subreddit refresh on login so the navigation list is ready sooner.
### Changed
- Streamlined the one-click join flow for `r/ReddixTUI` with clearer status messaging.

## [0.1.3] - 2025-10-05
### Added
- GitHub-backed update checker, in-app banner, and subreddit subscription helpers.
### Changed
- Refreshed README copy and screenshots to match the guided setup.

## [0.1.2] - 2025-10-04
### Added
- Enabled cargo-dist shell installers in the release workflow.

## [0.1.1] - 2025-10-04
### Added
- Persisted media previews so cached thumbnails load instantly.

## [0.1.0] - 2025-10-04
### Added
- Initial release with the polished login workflow, refreshed caching, and improved feed pagination.

[Unreleased]: https://github.com/ck-zhang/reddix/compare/v0.2.3...HEAD
[0.2.3]: https://github.com/ck-zhang/reddix/compare/v0.2.2...v0.2.3
[0.2.2]: https://github.com/ck-zhang/reddix/compare/v0.2.1...v0.2.2
[0.1.21]: https://github.com/ck-zhang/reddix/compare/v0.1.20...v0.1.21
[0.1.20]: https://github.com/ck-zhang/reddix/compare/v0.1.19...v0.1.20
[0.1.19]: https://github.com/ck-zhang/reddix/compare/v0.1.18...v0.1.19
[0.1.18]: https://github.com/ck-zhang/reddix/compare/v0.1.17...v0.1.18
[0.1.17]: https://github.com/ck-zhang/reddix/compare/v0.1.16...v0.1.17
[0.1.16]: https://github.com/ck-zhang/reddix/compare/v0.1.15...v0.1.16
[0.1.15]: https://github.com/ck-zhang/reddix/compare/v0.1.14...v0.1.15
[0.1.14]: https://github.com/ck-zhang/reddix/compare/v0.1.13...v0.1.14
[0.1.13]: https://github.com/ck-zhang/reddix/compare/v0.1.12...v0.1.13
[0.1.12]: https://github.com/ck-zhang/reddix/compare/v0.1.11...v0.1.12
[0.1.11]: https://github.com/ck-zhang/reddix/compare/v0.1.10...v0.1.11
[0.1.10]: https://github.com/ck-zhang/reddix/compare/v0.1.9...v0.1.10
[0.1.9]: https://github.com/ck-zhang/reddix/compare/v0.1.8...v0.1.9
[0.1.8]: https://github.com/ck-zhang/reddix/compare/v0.1.7...v0.1.8
[0.1.7]: https://github.com/ck-zhang/reddix/compare/v0.1.6...v0.1.7
[0.1.6]: https://github.com/ck-zhang/reddix/compare/v0.1.5...v0.1.6
[0.1.5]: https://github.com/ck-zhang/reddix/compare/v0.1.4...v0.1.5
[0.1.4]: https://github.com/ck-zhang/reddix/compare/v0.1.3...v0.1.4
[0.1.3]: https://github.com/ck-zhang/reddix/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/ck-zhang/reddix/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/ck-zhang/reddix/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/ck-zhang/reddix/releases/tag/v0.1.0
