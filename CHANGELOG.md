# Changelog

This file gives the important project changes. The project uses [Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.1.0] - 2026-08-20

### Added

- `record` starts system audio capture without a source menu.
- WASAPI supplies loopback audio. Media Foundation writes the MP3.
- The output profiles are 128, 192, 256, and 320 kbps stereo.
- Ratatui shows waveforms, level meters, a spectrum, and a split view.
- The TUI has pause and help controls.
- The CLI has headless operation, timed capture, file protection, and `doctor` checks.
- The project has a startup benchmark and Windows release automation.
- The release has a standalone executable and a one-command installer.
- The installer adds its directory to the current shell and the persistent user `PATH`.
- Supported terminals show a clickable link for the saved MP3.
