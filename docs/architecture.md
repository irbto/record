# Architecture

## Crates

The repository is a Cargo workspace.

| Crate | Path | Contents |
|---|---|---|
| `record-core` | `crates/record-core` | The capture engines: audio, video, screenshots, uploads, stored settings, and the shared color theme. It has no user interface |
| `record-tui` | the repository root | The `record` executable: command-line parsing and the terminal interface |
| `record-desktop` | `crates/record-desktop` | The `record-desktop` executable: the Tauri window and its web interface |

A user interface sends requests to an engine and draws the events that come back. It does not own capture state. This rule lets the terminal program and the desktop program use the same engines.

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

## Screenshots

`record screenshot` has three front ends: the terminal hub, the tray process, and direct capture. All three call the same capture function.

A capture duplicates one DXGI output, copies a single frame to a staging texture, and stops duplication. Fullscreen, window, and fixed-region captures read every output and join the images into one virtual desktop before they crop.

An HDR output gives a half-float scRGB surface through `IDXGIOutput5::DuplicateOutput1`. The `color` module divides each pixel by the SDR white level from `DisplayConfigGetDeviceInfo` and encodes sRGB. An SDR output gives 8-bit BGRA, which needs no conversion.

The `editor` module owns the frozen image, the annotations, the selection, and the toolbar. It has no Windows dependency. It receives pointer and key values and draws complete frames into a pixel buffer.

The `overlay` module owns a borderless topmost window on the captured monitor. It translates window messages to editor calls and copies each frame to the screen in `WM_PAINT`. GDI renders toolbar labels and typed text to coverage masks.

The `hub` module owns one thread with a Windows message queue. Hotkeys, tray menu commands, timer ticks, and requests from the terminal hub arrive in that queue. The thread runs one capture at a time and drops the input that arrives during a capture.

The `hotkeys` module holds the five global hotkeys as text such as `Ctrl+Shift+F8` and as the modifier bits and key code that `RegisterHotKey` takes. It calls no Windows function. The settings file stores two lines for each hotkey: the keys and the switch. A file without these lines gives the default keys. The hub thread registers the hotkeys from the shared settings. A `ReloadHotkeys` request releases and registers them again, and each registration reports the state of every hotkey. The terminal hub registers only the three screenshot hotkeys. The desktop program also registers the two video hotkeys. The hub thread records no video, so it passes a video hotkey to its owner as an event.

The `select` and `picker` modules are the plain region selection. The picker window shows the frozen image of one monitor without a toolbar. A mouse release or Enter confirms the dragged rectangle, and Esc cancels. The `region` module of `record-core` maps the rectangle to the monitor that holds most of it and makes its size even, because the video engine crops in the pixels of one monitor.

The `ui` module draws the terminal hub with Ratatui. It sends requests to the hub thread and shows the reports that come back. The tray process runs the same message loop on its main thread and has no terminal.

The saved image file and the clipboard bitmap come from the annotated image, cropped to the selection. They never contain the toolbar or the selection dimming.

The `upload` module reads ShareX custom uploader files, builds the request body, and reads the link from the response. It has no Windows dependency and no network access. The `http` module sends the request with WinHTTP, which uses the system proxy settings and certificate store.

## Desktop program

The desktop program is a Tauri window. The engines stay in Rust, and the interface is a web page in `crates/record-desktop/ui` that Tauri embeds in the executable.

The interface never touches a file or a device. It calls the functions of the `commands` and `recording` modules and listens for four events: `sound`, `video`, `shots`, and `hotkeys`. The `payload` module defines every value that crosses this boundary, and `ui/src/types.ts` mirrors it.

The `recording` module starts, stops, and discards the recordings. Audio and video each have one slot, so one audio recording and one video recording can run at the same time. The global hotkeys and the tray menu start and stop recordings without the interface. Thus, every start sends a `starting` event and every end sends a `stopped` event, and the interface follows these events. A start command returns the output path at once. A discard stops the recording and deletes the MP4 file or the whole audio session folder.

A start can wait for a countdown. The `countdown` module of `record-core` computes each frame as premultiplied BGRA pixels: a moving gradient along the four screen edges and the seconds that are left as seven-segment digits. A topmost layered window shows the frames with `UpdateLayeredWindow` and lets mouse input pass through. The window closes before the engine starts, and it is excluded from screen capture. A stop during the wait cancels the start.

The `engines` module runs the recordings. Each recording runs on its own thread. A second thread translates engine events to updates, so capture never waits for drawing. An audio update carries the extent of one sample block and 24 spectrum bands, about 47 times each second.

Every command runs away from the window thread. A command that receives a path accepts it only when the file lies in one of the three output folders. The file explorer may also show the folder of an audio session.

The `shell` module of `record-core` opens, shows, and recycles files with the shell functions `ShellExecuteW`, `SHOpenFolderAndSelectItems`, and `SHFileOperationW`. It reads thumbnails with `IShellItemImageFactory` and returns them as PNG. Each call runs on a new thread with its own COM apartment. The program does not start `explorer.exe` with arguments, because the explorer does not read them reliably.

The preview of the screen capture screen does not come from the recorder. While that screen is open, the interface asks a few times each second for a picture of the selected monitor. The `glance` module of `record-core` reads the desktop with GDI, scales it down, and the command returns a JPEG. The preview therefore works before a recording starts and on a still desktop.

The screenshot screen uses the same hub thread as the terminal hub. The window minimizes before a capture that it started and returns when the capture ends.

The window has no system frame. The interface draws the title bar, Tauri moves the window when the pointer drags the empty part of it, and the three window buttons call commands.

The `editing` module holds what clips and edits need between two calls. An audio recording is a session, so a clip request closes the current part under a name, and the temporary PCM source of that clip stays available for a trim. The video editor keeps one decoder thread for the open file. The interface asks it for single pictures as JPEG and reads the audio envelope once. An export closes the decoder, runs the trim of `record-core` on its own thread with progress events, and opens the new file again. An index in the settings folder remembers the encoding of each recorded video, because the trim needs it.

The program keeps a tray icon and runs as one copy. The `tray` module draws the icon for each state from the idle picture: an audio recording turns the pulse red, and a video recording adds a red disc. The tooltip and the tray menu follow the same state. The close button hides the window unless the user turned that off. A second start reaches the first copy through the single-instance plugin: it shows the window, or with `--quit` it ends the program. Every recording that runs when the program quits saves its file first. The program exits when the last engine reports the end of its recording.

The interface is TypeScript without a framework. `main.ts` holds the state and the screens, `menu.ts` the menu bar, `draw.ts` the canvas drawing, and `bridge.ts` the calls into Rust. The address of a screen is a hash route such as `#/audio`. `demo.ts` is a simulation of the engines for a plain browser, so the interface can be seen and tested without Windows. The colors are the palette of `record-core`, which the terminal interface also uses.
