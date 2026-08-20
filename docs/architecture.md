# Architecture

`record` uses native Windows audio components. Audio capture and terminal rendering use separate paths.

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

The worker initializes COM and Media Foundation. It opens the default `eRender` and `eConsole` endpoint.

The worker opens the MP3 writer. It then configures and starts an event-driven WASAPI loopback client.

After `IAudioClient::Start` succeeds, the worker sends `AudioEvent::Started`. This event changes the TUI state from `STARTING` to `RECORDING`.

The worker joins the Windows `Audio` MMCSS class during capture.

WASAPI supplies the endpoint mix format. The converter accepts PCM and IEEE float formats. It also accepts `WAVEFORMATEXTENSIBLE`.

The converter uses speaker positions to make stereo audio. A stateful linear resampler selects a supported MP3 rate.

The usual output rate is 48 kHz or 44.1 kHz. Media Foundation selects the highest available stereo profile at the requested bitrate or less.

The writer gives each PCM block a contiguous timestamp. It sends each block to `IMFSinkWriter`.

## UI isolation

Visualization blocks move through a bounded channel with `try_send`. If the channel is full, the worker discards a visualization block. Audio encoding continues.

Start and finalize messages use the same channel. The channel cannot make an unlimited backlog.

The waveform history grows as necessary. The program creates the RustFFT planner and spectrum buffers only after you select a spectrum view.

Pause stops new samples from entering the encoded timeline. Thus, the MP3 does not contain a silent pause gap.

## Startup sequence

1. The no-argument path makes the default configuration without Clap.
2. The main thread prepares the output path and the `Ctrl+C` handler.
3. The main thread starts the audio worker before it starts Ratatui.
4. The worker opens the MP3 writer and starts the WASAPI client.
5. The worker sends `AudioEvent::Started`.
6. The TUI changes from `STARTING` to `RECORDING`.

`scripts/benchmark-startup.ps1` measures the `AudioEvent::Started` boundary. This is the primary product metric because WASAPI capture is active at this boundary.

The script also measures the process time for `record --version`.

## Failure and file semantics

`record` does not replace an existing file unless you use `--force`. The sink writer finalizes the MP3 before a successful exit.

If capture or encoding fails, `record` removes the incomplete file. RAII guards stop the audio client and leave MMCSS.

Other guards stop Media Foundation, uninitialize COM, and restore the terminal state.
