# Changelog

This file gives the important project changes. The project uses [Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.3.2] - 2026-08-22

### Fixed

- Sixel video frames now use synchronized terminal updates, preventing the
  clear-before-draw flash that caused live and editor previews to flicker.
- Video capture now retains the DXGI surface until its GPU copy finishes and
  reuses one staging texture and NV12 buffer instead of allocating 4K resources
  for every frame. A 2560x1440 60 fps stress capture dropped from roughly 910
  MiB to 190 MiB of private memory and stayed flat beyond the reported crash.
- The H.264 sink now requests hardware and low-latency transforms. NVIDIA NVENC
  was verified active locally; the same Media Foundation path selects AMD or
  Intel hardware encoders when their drivers expose them.

## [0.3.1] - 2026-08-22

### Added

- `record video` shows a real Sixel raster of the captured desktop in Windows
  Terminal while capture is running.
- Stopping video capture opens an integrated frame-selection editor that uses
  the existing start/end clip handles and displays decoded MP4 frames.
- Saving a video clip rebuilds the selected H.264/AAC range through Media
  Foundation and replaces the original only after the new MP4 finalizes.

### Changed

- Removed the separate `record review` command and its half-block renderer.
- The minimum supported Rust version is now 1.90 for the raster dependency.

## [0.3.0] - 2026-08-22

### Added

- `record video` captures the primary monitor and system audio to H.264/AAC MP4.
- `--monitor` selects the primary monitor, an output index, or lists every monitor.
- `--fit` selects contain, cover, stretch, or native source mapping.
- `--canvas` selects a WxH size or a named preset such as 1080p.
- `--crop` selects a LEFT,TOP,WIDTH,HEIGHT source rectangle.
- Startup measurements print capture ready, recording ready, and finalize times.
- Preview clamps the playback range to the device media length.
- `record video` runs the same full-screen Ratatui interface as audio capture,
  with `--no-tui` keeping the line output for scripts.
- Merging a version bump to `main` now tags the release automatically once tests pass, which publishes the Windows exe through the existing release workflow.

### Fixed

- The default crop no longer collapses the native canvas to a 2x2 frame that
  Media Foundation rejected with 0xC00D36B4 before capture started.
- Video samples carry explicit durations, fixing the 0xC00D36C9 write failure
  that stopped every recording at the first frame.
- Video timestamps follow a recorded-time clock that freezes during pauses, so
  pause spans stay aligned with the self-clocked audio timeline.
- Idle screens emit an initial black frame so `--duration` still completes when
  the desktop never updates.

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
