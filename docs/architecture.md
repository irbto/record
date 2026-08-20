# Architecture

`record` keeps the capture path native and isolates it from terminal work.

```text
default Windows playback endpoint
              │
       WASAPI shared-mode loopback
              │
     format decode + stereo downmix
              │
       linear rate conversion
              │
        PCM16 stereo frames
         ┌────┴───────────────┐
         │                    │
Media Foundation MP3    bounded try-send
         │                    │
      output.mp3       waveform / lazy FFT
                              │
                         Ratatui frame
```

## Capture thread

The worker initializes multithreaded COM, Media Foundation, the default `eRender` / `eConsole` endpoint, and an event-driven WASAPI loopback client. It joins Windows' `Audio` MMCSS class while capturing.

WASAPI supplies the endpoint mix format. The converter supports PCM and IEEE float wave formats, including `WAVEFORMATEXTENSIBLE`, and performs speaker-aware stereo downmixing. A small stateful linear resampler converts to the nearest supported MP3 rate, normally 48 kHz or 44.1 kHz.

Media Foundation selects the highest available stereo MP3 profile at or below the requested bitrate. PCM samples are timestamped on a contiguous encoded timeline and sent to `IMFSinkWriter`.

## UI isolation

Visualization blocks travel over a bounded channel with `try_send`. If a terminal cannot keep up, UI frames are dropped while audio encoding continues. Start and finalize state messages use the same channel, but never compete with an unbounded visualization backlog.

The default waveform is incremental. The RustFFT planner and spectrum buffers are created only after the user presses `W`. Pausing stops samples from entering the encoded timeline, so the output has no silent pause gap.

## Startup sequence

1. The bare no-argument path constructs defaults without invoking Clap.
2. The output path and Ctrl+C handler are prepared.
3. The audio worker starts and opens WASAPI plus the encoder.
4. Only then does the main thread mount Ratatui.

`scripts/benchmark-startup.ps1` measures normal CLI process time and an internal timestamp at the `AudioEvent::Started` boundary. The latter is the useful product metric: time from process entry until WASAPI is actively capturing.

## Failure and file semantics

Existing files are protected unless `--force` is explicit. The sink writer is finalized before a successful exit. If capture or encoding fails, the partially written destination is removed. RAII guards stop the audio client, leave MMCSS, shut down Media Foundation, uninitialize COM, and restore terminal state.
