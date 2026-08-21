# Architecture

`record` uses native Windows audio components. The audio worker and terminal renderer use separate data paths.

```text
default Windows playback endpoint
                |
       WASAPI shared loopback
                |
       decode and stereo mix
                |
        linear rate converter
                |
          PCM16 stereo frames
           /             \
  OutputManager       bounded try_send
       |                     |
  MP3 session part     waveform and lazy FFT
       |                     |
temporary PCM cache          TUI
       |                     |
       +------ named clip ---+
                   |
          preview or trim worker
                   |
          finalized temporary MP3
                   |
              ReplaceFileW
```

## Capture thread

The worker initializes COM and Media Foundation. It opens the default `eRender` and `eConsole` endpoint.

The worker opens the first MP3 writer. A session also opens a temporary PCM cache for the active part.

The worker configures and starts an event-driven WASAPI loopback client. It then sends `AudioEvent::Started`.

The TUI changes from `STARTING` to `RECORDING` only after this event. Thus, `RECORDING` means that system audio capture is active.

The worker joins the Windows `Audio` MMCSS class during capture.

WASAPI supplies the endpoint mix format. The converter accepts PCM, IEEE float, and `WAVEFORMATEXTENSIBLE` data.

The converter uses Windows speaker positions to create stereo audio. A stateful linear converter selects a supported MP3 sample rate.

The usual output rate is 48 kHz or 44.1 kHz. Media Foundation selects the highest available stereo profile at the requested bit rate or less.

The writer gives each PCM block a contiguous timestamp. It sends each block to `IMFSinkWriter`.

## Sessions and exact rotation

A bare `record` command creates one `recording-TIMESTAMP` directory. `part-001.mp3` opens before WASAPI starts.

The default part limit is ten minutes. `segment_frame_limit` converts this duration to an integer frame count.

`OutputManager` can split one converted block across a boundary. The first writer gets the exact remaining frames. The next writer gets the rest.

The WASAPI client stays active while writers change. Rotation does not reopen the audio device or reset the process timeline.

An explicit `-o FILE.mp3` target uses one writer. It does not rotate and does not create a PCM edit cache.

## Named clips

The command channel carries `AudioCommand::SaveClip`. The audio worker handles commands between capture waits.

A clip command finalizes the active part and gives it a safe user name. Capture continues in the next numbered part.

Each session part has an interleaved stereo PCM16 cache while it is active. Automatic parts delete this cache after finalization.

A named clip keeps its cache with an `Arc` lease. The final lease removes the process-specific cache directory.

The clip editor reads the cache in fixed blocks. It stores only a bounded minimum and maximum waveform for each display bin.

The start and end handles use absolute PCM frame positions. The editor keeps a minimum selection of 50 milliseconds.

## Preview and trim

Windows MCI plays only the selected MP3 time range. Preview does not start an external process or require another codec package.

The TUI pauses capture during preview. This prevents the loopback stream from recording its own playback.

A helper thread performs a trim. It encodes the selected PCM range to a hidden temporary MP3 beside the clip.

Media Foundation finalizes and closes the temporary MP3 first. `ReplaceFileW` then replaces the original clip with write-through behavior.

An encoding or replacement error keeps the original clip. The TUI shows the error and keeps the editor open.

## UI isolation

Visualization blocks move through a bounded channel with `try_send`. The worker discards a block when the display channel is busy.

The sender reserves eight channel slots for state events. A slow terminal cannot hide start, save, or finalization state behind sample blocks.

The waveform history starts with a small allocation. It grows only as captured samples fill the six-second scope.

The TUI creates the FFT planner after the first sample block for a spectrum view. Split view is active by default.

Pause omits packets from the encoded timeline. The MP3 does not contain a silent pause gap.

## Startup sequence

1. The no-argument path creates the default configuration without Clap.
2. The main thread creates a unique session directory and installs the `Ctrl+C` handler.
3. The main thread starts the audio worker before it starts Ratatui.
4. The worker opens the first MP3 and PCM cache files.
5. The worker starts the WASAPI client.
6. The worker sends `AudioEvent::Started`.
7. The TUI changes from `STARTING` to `RECORDING`.

`scripts/benchmark-startup.ps1` starts the binary without arguments. An internal environment probe stops it at the `AudioEvent::Started` boundary.

The script measures session creation, writer creation, and active WASAPI capture. This is the primary product metric.

## Failure and file semantics

An explicit output does not replace an existing file unless the user specifies `--force`.

The sink writer finalizes each MP3 before it sends a save event. An incomplete writer removes its output file during drop.

WASAPI requires one `ReleaseBuffer` call after each successful `GetBuffer` call. The packet loop saves processing errors until the release is complete.

RAII guards stop the audio client and leave MMCSS. Other guards stop Media Foundation, uninitialize COM, close handles, and restore the terminal.
