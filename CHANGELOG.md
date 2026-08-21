# Changelog

This file gives the important project changes. The project uses [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- `record video` captures the primary monitor and system audio to H.264/AAC MP4.
- `--monitor` selects the primary monitor, an output index, or lists every monitor.
- `--fit` selects contain, cover, stretch, or native source mapping.
- `--canvas` selects a WxH size or a named preset such as 1080p.
- `--crop` selects a LEFT,TOP,WIDTH,HEIGHT source rectangle.
- Startup measurements print capture ready, recording ready, and finalize times.
- Preview clamps the playback range to the device media length.

## [0.2.0] - 2026-08-20

### Added

- A bare `record` command creates one grouped session folder.
- Sessions start a new MP3 at exact ten-minute boundaries by default.
- The `--part-minutes` option changes the automatic part length.
- `C` prompts for a clip name and continues capture in a new part.
- The TUI shows finalized parts and clips in a saves panel.
- `E` opens a named clip with complete left and right waveforms.
- The clip editor supports frame-bounded trimming and selected-range preview.
- Preview pauses capture and restores the prior pause state after playback.
- The source has file-level design documentation and 90 unit tests.

### Changed

- Split view is the default TUI view.
- The startup benchmark now measures the complete bare-command session path.
- Explicit `-o FILE.mp3` keeps the single-file behavior.

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
