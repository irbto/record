# record

> Type `record`. Capture starts. Press `Ctrl+C`. Get an MP3.

[![CI](https://github.com/irbto/record/actions/workflows/ci.yml/badge.svg)](https://github.com/irbto/record/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-7c3aed.svg)](LICENSE)
[![Windows](https://img.shields.io/badge/Windows-10%20%7C%2011-06b6d4.svg)](#platform-support)

`record` is a fast, native terminal recorder for Windows system audio. It records the mix playing through your default output device—not your microphone—and writes a high-quality MP3 without bundling FFmpeg or opening an arm screen first.

```text
╭ record  SYSTEM AUDIO ───────────────────────────────────────────────╮
│              ● RECORDING   00:01:42   320 kbps · 48 kHz   WAVE    │
╰─────────────────────────────────────────────────────────────────────╯
╭ LEFT · 6 SECOND SCOPE ─────────────────────────────────────────────╮
│  ⠀⠀⠀⣀⣤⣶⣄⡀⠀⠀⣠⣾⣿⣷⣄⠀⠀⠀⣀⣴⣶⣤⣀⠀⠀⣠⣾⣿⣷⣄⠀⠀               │
╰─────────────────────────────────────────────────────────────────────╯
╭ RIGHT · 6 SECOND SCOPE ────────────────────────────────────────────╮
│  ⠀⣀⣴⣿⣷⣦⣀⠀⠀⣠⣶⣶⣄⠀⠀⠀⣀⣴⣿⣿⣦⣀⠀⠀⣠⣶⣶⣄⠀⠀                 │
╰─────────────────────────────────────────────────────────────────────╯
  Ctrl+C / S  save & stop    Space  pause    W  view    ?  help
```

## Why this exists

Most desktop-audio recording flows make you pick a source, mount a UI, configure an encoder, and export afterward. `record` treats recording like a shell primitive:

```powershell
record
```

The default path has no menu and no mandatory configuration. Capture begins before the full-screen TUI is mounted.

## Highlights

- Captures every ordinary app routed to the default Windows playback device through WASAPI loopback.
- Encodes directly to stereo MP3 with Windows Media Foundation; 320 kbps is the default.
- Shows live left/right braille waveforms, level meters, a lazy-loaded spectrum, and a split view.
- Starts recording immediately and always finalizes cleanly on `Ctrl+C`, `S`, `Q`, or `Esc`.
- Supports pause/resume, automatic stop durations, explicit output paths, and headless scripts.
- Ships as one native executable with no FFmpeg, Node.js, WebView, or runtime service.

## Install

With a Rust toolchain:

```powershell
cargo install --git https://github.com/irbto/record --locked
```

Prebuilt releases can be installed with PowerShell:

```powershell
irm https://raw.githubusercontent.com/irbto/record/main/install.ps1 | iex
```

Then start from any terminal:

```powershell
record
```

## Controls

| Key | Action |
|---|---|
| `Ctrl+C`, `S`, `Q`, `Esc` | Stop, finalize the MP3, and exit |
| `Space` | Pause or resume; paused time is omitted |
| `W` | Cycle waveform, spectrum, and split views |
| `?` | Toggle quick help |

## CLI

```text
record [OPTIONS]
record doctor

Options:
  -o, --output <FILE>       Output path (adds .mp3 when omitted)
  -b, --bitrate <BITRATE>   k128, k192, k256, or k320 [default: k320]
  -d, --duration <SECONDS>  Stop automatically, including fractional seconds
      --no-tui              Use line-oriented output for scripts or pipes
  -f, --force               Replace an existing output file
```

Examples:

```powershell
# Record until Ctrl+C and choose a timestamped filename.
record

# Record a 30-second, 192 kbps clip.
record -d 30 -b k192 -o demo.mp3

# Verify the endpoint and native encoder without recording.
record doctor
```

## Fast by design

The no-argument path bypasses the general CLI parser, starts the dedicated audio thread before terminal mounting, grows the waveform history incrementally, and does not create an FFT planner until the spectrum view is requested. The audio thread uses a bounded, non-blocking visualization channel so terminal rendering cannot stall capture.

Run the included benchmark to measure both CLI launch and time-to-WASAPI-ready on your machine:

```powershell
.\scripts\benchmark-startup.ps1 -Runs 30
```

See [the benchmark notes](benchmarks/README.md) for metric definitions and the current reference result, and [the architecture notes](docs/architecture.md) for the data path and performance boundaries.

## Platform support

Windows 10 and Windows 11 are the supported targets today. The core and TUI are portable, and macOS ScreenCaptureKit/Core Audio plus Linux PipeWire backends are welcome.

“System audio” means the shared-mode mix sent to the current default playback endpoint. Audio routed to another device, protected media, exclusive-mode streams, and microphone input are outside that mix.

## Project status

This is an early open-source release. The Windows capture and native MP3 path are working and hardware-smoke-tested, but device switching, metadata, and additional operating systems remain on the roadmap.

Contributions are welcome—start with [CONTRIBUTING.md](CONTRIBUTING.md). Licensed under [MIT](LICENSE).
