use super::super::{AMBER, BORDER, CYAN, GREEN, PURPLE};
use super::render::{
    FadeColumns, TimelineColumns, progress_ratio, spinner_frame, video_waveform_lines,
};
use super::*;
use crate::clip::SelectionHandle;
use crate::video::audio_preview::{AudioPreviewCommand, AudioPreviewEvent};
use crossbeam_channel::bounded;
use ratatui::{Terminal, backend::TestBackend, style::Color};

fn video_app() -> VideoApp {
    VideoApp::new(
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
        PathBuf::from("recording-20260821-005218.mp4"),
    )
}

fn press_video(app: &mut VideoApp, code: KeyCode, modifiers: KeyModifiers) {
    app.handle_event(Event::Key(KeyEvent::new(code, modifiers)));
}

fn start_recording(app: &mut VideoApp) {
    let (sender, receiver) = bounded(8);
    sender
        .send(VideoEvent::Started {
            width: 2560,
            height: 1440,
            fps: 60,
            sample_rate: 48_000,
            capture_ready_ms: 148.0,
        })
        .unwrap();
    app.drain_video_events(&receiver);
}

fn attach_video_editor(app: &mut VideoApp) -> Receiver<PreviewCommand> {
    let (commands, command_receiver) = bounded(8);
    let (_event_sender, events) = bounded(8);
    let summary = VideoSummary {
        output: PathBuf::from("recording-20260821-005218.mp4"),
        width: 2_560,
        height: 1_440,
        fps: 60,
        frames: 5_400,
        sample_rate: 48_000,
        video_bitrate: 20_000_000,
        audio_bitrate: 192_000,
        recording_ready_ms: Some(148.0),
        finalize_ms: 80.0,
    };
    app.completed_summary = Some(summary.clone());
    app.editor = Some(VideoEditor {
        selection: ClipSelection::new(0, summary.frames, summary.fps).unwrap(),
        active_tool: EditorTool::RangeStart,
        edit_settings: VideoEditSettings::default(),
        summary,
        commands,
        events,
        decoder: None,
        format: Some(PreviewFormat {
            width: 2_560,
            height: 1_440,
            fps: 60.0,
            duration: Duration::from_secs(90),
        }),
        position: Duration::ZERO,
        picture_position: Duration::ZERO,
        audio_preview: None,
        audio_preview_error: None,
        playing: false,
        resume_pending: false,
        awaiting_picture: false,
        next_picture_due: Instant::now(),
        pending_seek: None,
        error: None,
        trim_job: None,
        waveform: Some(EditorWaveform::Unavailable(
            "Audio data not loaded".to_owned(),
        )),
    });
    command_receiver
}

fn buffer_row(buffer: &ratatui::buffer::Buffer, y: u16) -> String {
    (buffer.area.x..buffer.area.right())
        .map(|x| buffer[(x, y)].symbol())
        .collect()
}

#[test]
fn video_started_event_activates_recording_state() {
    let mut app = video_app();
    assert_eq!(app.width, 0);
    start_recording(&mut app);
    assert!(matches!(app.state, CaptureState::Recording));
    assert_eq!(app.width, 2560);
    assert_eq!(app.height, 1440);
    assert_eq!(app.fps, 60);
    assert_eq!(app.sample_rate, 48_000);
    assert_eq!(app.capture_ready_ms, Some(148.0));
}

#[test]
fn video_notice_and_finalizing_events_update_state() {
    let mut app = video_app();
    let (sender, receiver) = bounded(8);
    sender
        .send(VideoEvent::Notice("duplication reset".to_owned()))
        .unwrap();
    sender.send(VideoEvent::Finalizing).unwrap();
    app.drain_video_events(&receiver);
    assert_eq!(app.notice.as_deref(), Some("duplication reset"));
    assert!(matches!(app.state, CaptureState::Finalizing));
}

#[test]
fn video_space_toggles_pause() {
    let mut app = video_app();
    start_recording(&mut app);
    press_video(&mut app, KeyCode::Char(' '), KeyModifiers::NONE);
    assert!(app.paused.load(Ordering::Relaxed));
    assert!(matches!(app.state, CaptureState::Paused));
    press_video(&mut app, KeyCode::Char(' '), KeyModifiers::NONE);
    assert!(!app.paused.load(Ordering::Relaxed));
    assert!(matches!(app.state, CaptureState::Recording));
}

#[test]
fn video_clock_freezes_while_paused() {
    let mut app = video_app();
    start_recording(&mut app);
    app.tick -= Duration::from_secs(5);
    app.advance_recorded_time();
    assert!(app.recorded >= Duration::from_secs(4));
    let frozen = app.recorded;
    press_video(&mut app, KeyCode::Char(' '), KeyModifiers::NONE);
    app.tick -= Duration::from_secs(10);
    app.advance_recorded_time();
    assert_eq!(app.recorded, frozen);
}

#[test]
fn video_stop_keys_finalize_capture() {
    let mut app = video_app();
    start_recording(&mut app);
    press_video(&mut app, KeyCode::Char('s'), KeyModifiers::NONE);
    assert!(app.stop.load(Ordering::Relaxed));
    assert!(matches!(app.state, CaptureState::Finalizing));

    let mut app = video_app();
    press_video(
        &mut app,
        KeyCode::Char('C'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    );
    assert!(app.stop.load(Ordering::Relaxed));

    let mut app = video_app();
    press_video(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(app.stop.load(Ordering::Relaxed));
}

#[test]
fn video_help_overlay_opens_and_closes() {
    let mut app = video_app();
    press_video(&mut app, KeyCode::Char('?'), KeyModifiers::NONE);
    assert!(app.show_help);
    press_video(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(!app.show_help);
}

#[test]
fn video_view_renders_the_screen_capture_ui() {
    let mut app = video_app();
    start_recording(&mut app);
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| app.render(frame)).unwrap();
    let content = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(content.contains("SCREEN CAPTURE"));
    assert!(content.contains("RECORDING"));
    assert!(content.contains("2560x1440 @ 60 fps"));
    assert!(content.contains("Space"));
    assert!(content.contains(".mp4"));
}

#[test]
fn video_view_before_start_shows_the_preparing_state() {
    let mut app = video_app();
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| app.render(frame)).unwrap();
    let content = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(content.contains("STARTING"));
    assert!(content.contains("preparing encoder"));
}

#[test]
fn video_editor_reuses_clip_handles_and_frame_nudging() {
    let mut app = video_app();
    let commands = attach_video_editor(&mut app);
    press_video(&mut app, KeyCode::Right, KeyModifiers::CONTROL);
    let editor = app.editor.as_ref().unwrap();
    assert_eq!(editor.selection.start_frame(), 1);
    assert_eq!(
        commands.try_recv().unwrap(),
        PreviewCommand::Seek(frame_time(1, 60))
    );

    press_video(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    press_video(&mut app, KeyCode::Left, KeyModifiers::SHIFT);
    let editor = app.editor.as_ref().unwrap();
    assert_eq!(editor.selection.active_handle(), SelectionHandle::End);
    assert_eq!(editor.selection.end_frame(), 5_340);
}

#[test]
fn windows_backtab_selects_the_previous_editor_tool() {
    let mut app = video_app();
    let _commands = attach_video_editor(&mut app);

    press_video(&mut app, KeyCode::BackTab, KeyModifiers::SHIFT);

    assert_eq!(
        app.editor.as_ref().unwrap().active_tool,
        EditorTool::CropBottom
    );
}

#[test]
fn video_editor_shortcuts_select_and_adjust_independent_fades() {
    let mut app = video_app();
    let commands = attach_video_editor(&mut app);

    press_video(&mut app, KeyCode::Char('v'), KeyModifiers::NONE);
    assert_eq!(
        app.editor.as_ref().unwrap().active_tool,
        EditorTool::VideoFadeIn
    );
    assert_eq!(
        commands.try_recv().unwrap(),
        PreviewCommand::Seek(Duration::ZERO)
    );
    app.editor.as_mut().unwrap().awaiting_picture = false;

    press_video(&mut app, KeyCode::Right, KeyModifiers::NONE);
    let editor = app.editor.as_ref().unwrap();
    assert_eq!(
        editor.edit_settings.video_fade_in,
        Duration::from_millis(100)
    );
    assert_eq!(editor.position, Duration::from_millis(100));
    assert_eq!(
        commands.try_recv().unwrap(),
        PreviewCommand::Seek(Duration::from_millis(100))
    );
    app.editor.as_mut().unwrap().awaiting_picture = false;

    press_video(&mut app, KeyCode::Char('v'), KeyModifiers::NONE);
    assert_eq!(
        app.editor.as_ref().unwrap().active_tool,
        EditorTool::VideoFadeOut
    );
    let _ = commands.try_recv().unwrap();
    app.editor.as_mut().unwrap().awaiting_picture = false;
    press_video(&mut app, KeyCode::Right, KeyModifiers::SHIFT);
    assert_eq!(
        app.editor.as_ref().unwrap().edit_settings.video_fade_out,
        Duration::from_secs(1)
    );

    app.editor.as_mut().unwrap().awaiting_picture = false;
    let _ = commands.try_recv();
    press_video(&mut app, KeyCode::Char('a'), KeyModifiers::NONE);
    assert_eq!(
        app.editor.as_ref().unwrap().active_tool,
        EditorTool::AudioFadeIn
    );
    app.editor.as_mut().unwrap().awaiting_picture = false;
    let _ = commands.try_recv();
    press_video(&mut app, KeyCode::Up, KeyModifiers::NONE);
    assert_eq!(
        app.editor.as_ref().unwrap().edit_settings.audio_fade_in,
        Duration::from_millis(100)
    );
}

#[test]
fn crop_tools_move_physical_edges_in_even_pixel_steps_and_reset() {
    let mut app = video_app();
    let _commands = attach_video_editor(&mut app);

    press_video(&mut app, KeyCode::Char('c'), KeyModifiers::NONE);
    press_video(&mut app, KeyCode::Right, KeyModifiers::NONE);
    press_video(&mut app, KeyCode::Char('c'), KeyModifiers::NONE);
    press_video(&mut app, KeyCode::Left, KeyModifiers::NONE);
    press_video(&mut app, KeyCode::Char('c'), KeyModifiers::NONE);
    press_video(&mut app, KeyCode::Down, KeyModifiers::SHIFT);
    press_video(&mut app, KeyCode::Char('c'), KeyModifiers::NONE);
    press_video(&mut app, KeyCode::Up, KeyModifiers::NONE);

    let editor = app.editor.as_ref().unwrap();
    assert_eq!(editor.active_tool, EditorTool::CropBottom);
    assert_eq!(editor.edit_settings.crop_left, 2);
    assert_eq!(editor.edit_settings.crop_right, 2);
    assert_eq!(editor.edit_settings.crop_top, 20);
    assert_eq!(editor.edit_settings.crop_bottom, 2);
    let overlay = editor.raster_edit_overlay().unwrap();
    assert_eq!(overlay.active, Some(crate::raster::CropEdge::Bottom));

    press_video(&mut app, KeyCode::Char('r'), KeyModifiers::NONE);
    let editor = app.editor.as_ref().unwrap();
    assert_eq!(editor.active_tool, EditorTool::RangeStart);
    assert!(editor.edit_settings.is_identity());
}

#[test]
fn video_fade_gain_is_previewed_at_the_current_playhead() {
    let mut app = video_app();
    let _commands = attach_video_editor(&mut app);
    let editor = app.editor.as_mut().unwrap();
    editor.edit_settings.video_fade_in = Duration::from_secs(1);

    editor.position = Duration::ZERO;
    assert_eq!(editor.raster_edit_overlay().unwrap().video_gain, 0.0);
    editor.position = Duration::from_millis(500);
    assert_eq!(editor.raster_edit_overlay().unwrap().video_gain, 0.5);
    editor.position = Duration::from_secs(1);
    assert!(editor.raster_edit_overlay().is_none());
}

#[test]
fn video_space_pauses_and_resumes_from_the_current_frame() {
    let mut app = video_app();
    let commands = attach_video_editor(&mut app);
    let editor = app.editor.as_mut().unwrap();
    editor.position = Duration::from_secs(5);
    editor.playing = true;

    press_video(&mut app, KeyCode::Char(' '), KeyModifiers::NONE);
    assert!(!app.editor.as_ref().unwrap().playing);
    assert!(commands.is_empty());

    press_video(&mut app, KeyCode::Char(' '), KeyModifiers::NONE);
    let editor = app.editor.as_ref().unwrap();
    assert!(editor.playing);
    assert_eq!(editor.position, Duration::from_secs(5));
    assert!(commands.is_empty());

    app.request_editor_picture();
    assert_eq!(commands.try_recv().unwrap(), PreviewCommand::Step);
}

#[test]
fn silent_video_fallback_is_paced_to_the_source_frame_rate() {
    let mut app = video_app();
    let commands = attach_video_editor(&mut app);
    let editor = app.editor.as_mut().unwrap();
    editor.playing = true;
    editor.next_picture_due = Instant::now() + Duration::from_secs(1);

    app.request_editor_picture();
    assert!(commands.is_empty());

    app.editor.as_mut().unwrap().next_picture_due = Instant::now() - Duration::from_millis(1);
    app.request_editor_picture();
    assert_eq!(commands.try_recv().unwrap(), PreviewCommand::Step);
    assert!(app.editor.as_ref().unwrap().next_picture_due > Instant::now());
}

#[test]
fn video_space_restarts_only_after_reaching_the_end_marker() {
    let mut app = video_app();
    let commands = attach_video_editor(&mut app);
    app.editor.as_mut().unwrap().position = Duration::from_secs(90);

    press_video(&mut app, KeyCode::Char(' '), KeyModifiers::NONE);
    let editor = app.editor.as_ref().unwrap();
    assert!(editor.playing);
    assert_eq!(editor.position, Duration::ZERO);
    assert_eq!(
        commands.try_recv().unwrap(),
        PreviewCommand::Seek(Duration::ZERO)
    );
}

#[test]
fn video_space_starts_from_the_beginning_after_scrubbing_to_the_end() {
    let mut app = video_app();
    let commands = attach_video_editor(&mut app);

    press_video(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert!(matches!(commands.try_recv(), Ok(PreviewCommand::Seek(_))));
    press_video(&mut app, KeyCode::Char(' '), KeyModifiers::NONE);

    let editor = app.editor.as_ref().unwrap();
    assert!(editor.playing);
    assert!(!editor.resume_pending);
    assert_eq!(editor.position, Duration::ZERO);
    assert_eq!(editor.pending_seek, Some(Duration::ZERO));
}

#[test]
fn video_backspace_pauses_at_the_start_marker() {
    let mut app = video_app();
    let commands = attach_video_editor(&mut app);
    let editor = app.editor.as_mut().unwrap();
    editor.selection.nudge(60);
    editor.position = Duration::from_secs(5);
    editor.playing = true;

    press_video(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
    let editor = app.editor.as_ref().unwrap();
    assert!(!editor.playing);
    assert_eq!(editor.position, Duration::from_secs(1));
    assert_eq!(editor.selection.start_frame(), 60);
    assert_eq!(
        commands.try_recv().unwrap(),
        PreviewCommand::Seek(Duration::from_secs(1))
    );
}

#[test]
fn native_audio_pauses_for_edits_and_keeps_only_deliberate_resume_intent() {
    let mut app = video_app();
    let _commands = attach_video_editor(&mut app);
    let (preview, audio_commands, audio_events) = EditorAudioPreview::test_channels();
    let editor = app.editor.as_mut().unwrap();
    editor.audio_preview = Some(preview);
    editor.playing = true;
    editor.position = Duration::from_secs(5);

    press_video(&mut app, KeyCode::Char('c'), KeyModifiers::NONE);
    assert_eq!(
        audio_commands.try_recv().unwrap(),
        AudioPreviewCommand::PauseForEdit
    );
    assert!(!app.editor.as_ref().unwrap().playing);
    assert!(!app.editor.as_ref().unwrap().resume_pending);

    let editor = app.editor.as_mut().unwrap();
    editor.playing = true;
    editor.audio_preview.as_mut().unwrap().playing = true;
    press_video(&mut app, KeyCode::Char(' '), KeyModifiers::NONE);
    assert_eq!(
        audio_commands.try_recv().unwrap(),
        AudioPreviewCommand::Pause
    );
    assert!(app.editor.as_ref().unwrap().resume_pending);

    audio_events
        .send(AudioPreviewEvent::Paused {
            position: Duration::from_secs(6),
        })
        .unwrap();
    app.drain_audio_preview_events();
    assert!(app.editor.as_ref().unwrap().resume_pending);
    assert_eq!(
        app.editor.as_ref().unwrap().position,
        Duration::from_secs(6)
    );
}

#[test]
fn video_trim_closes_a_full_preview_queue_without_blocking_the_tui() {
    let mut app = video_app();
    let commands = attach_video_editor(&mut app);
    let (release_sender, release_receiver) = bounded::<()>(0);
    let decoder = thread::spawn(move || {
        release_receiver.recv().unwrap();
        while let Ok(command) = commands.recv() {
            if command == PreviewCommand::Close {
                break;
            }
        }
    });
    let editor = app.editor.as_mut().unwrap();
    editor.decoder = Some(decoder);
    editor.selection.nudge(1);
    for frame in 0..8 {
        editor
            .commands
            .try_send(PreviewCommand::Seek(frame_time(frame, 60)))
            .unwrap();
    }

    press_video(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(app.editor.as_ref().unwrap().trim_job.is_some());
    release_sender.send(()).unwrap();

    let timeout = Instant::now() + Duration::from_secs(2);
    while !app
        .editor
        .as_ref()
        .and_then(|editor| editor.trim_job.as_ref())
        .is_some_and(|job| job.worker.is_finished())
    {
        assert!(Instant::now() < timeout, "trim coordinator stayed blocked");
        thread::yield_now();
    }
    app.poll_video_trim();
    let editor = app.editor.as_ref().unwrap();
    assert!(editor.trim_job.is_none());
    assert!(editor.decoder.is_some());
    assert!(editor.waveform.is_some());
    assert!(
        editor
            .error
            .as_deref()
            .is_some_and(|error| error.contains("Could not save the trimmed video"))
    );
}

#[test]
fn video_editor_header_keeps_summary_on_one_responsive_line() {
    for width in [100, 60] {
        let mut app = video_app();
        let _commands = attach_video_editor(&mut app);
        let backend = TestBackend::new(width, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        let summary = buffer_row(terminal.backend().buffer(), 1);
        assert!(summary.contains("SELECTING"));
        assert!(summary.contains("00:00:00"));
        assert!(summary.contains("00:01:30"));
        assert!(summary.contains("ACTIVE"));
        assert!(!buffer_row(terminal.backend().buffer(), 2).contains("ACTIVE"));
    }
}

#[test]
fn video_range_uses_the_active_side_as_the_thick_bar() {
    let mut app = video_app();
    let _commands = attach_video_editor(&mut app);
    let backend = TestBackend::new(100, 30);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| app.render(frame)).unwrap();
    let buffer = terminal.backend().buffer();
    assert_eq!(buffer[(1, 16)].symbol(), "▌");
    assert_eq!(buffer[(1, 16)].fg, AMBER);
    assert_eq!(buffer[(98, 16)].symbol(), "▕");
    assert_ne!(buffer[(1, 16)].symbol(), "◆");

    press_video(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    terminal.draw(|frame| app.render(frame)).unwrap();
    let buffer = terminal.backend().buffer();
    assert_eq!(buffer[(1, 16)].symbol(), "▏");
    assert_eq!(buffer[(98, 16)].symbol(), "▐");
    assert_eq!(buffer[(98, 16)].fg, AMBER);

    app.editor.as_mut().unwrap().active_tool = EditorTool::VideoFadeIn;
    terminal.draw(|frame| app.render(frame)).unwrap();
    let buffer = terminal.backend().buffer();
    assert_eq!(buffer[(1, 16)].symbol(), "▏");
    assert_eq!(buffer[(1, 16)].fg, CYAN);
    assert_eq!(buffer[(98, 16)].symbol(), "▕");
    assert_eq!(buffer[(98, 16)].fg, GREEN);

    let editor = app.editor.as_mut().unwrap();
    editor.active_tool = EditorTool::RangeEnd;
    editor.selection.toggle_handle();
    editor.selection.nudge(5_397);
    editor.active_tool = EditorTool::RangeStart;
    terminal.draw(|frame| app.render(frame)).unwrap();
    assert_eq!(terminal.backend().buffer()[(98, 16)].symbol(), "▌");
    app.editor.as_mut().unwrap().active_tool = EditorTool::RangeEnd;
    terminal.draw(|frame| app.render(frame)).unwrap();
    assert_eq!(terminal.backend().buffer()[(98, 16)].symbol(), "▐");
}

#[test]
fn video_waveform_uses_the_same_timeline_columns_as_clip_range() {
    let mut app = video_app();
    let _commands = attach_video_editor(&mut app);
    app.editor.as_mut().unwrap().position = Duration::from_secs(45);
    let backend = TestBackend::new(100, 30);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| app.render(frame)).unwrap();
    let buffer = terminal.backend().buffer();

    assert!(buffer_row(buffer, 19).contains("WAVEFORM"));
    assert!(buffer_row(buffer, 19).contains("Audio data not loaded"));
    assert_eq!(buffer[(1, 16)].symbol(), "▌");
    assert_eq!(buffer[(98, 16)].symbol(), "▕");
    assert_eq!(buffer[(50, 16)].symbol(), "◆");
    for row in 20..=22 {
        assert_eq!(buffer[(1, row)].symbol(), "│");
        assert_eq!(buffer[(98, row)].symbol(), "│");
        assert_eq!(buffer[(50, row)].symbol(), "│");
        assert_eq!(buffer[(50, row)].fg, Color::White);
    }
}

#[test]
fn waveform_lines_style_selection_playhead_and_optional_fades() {
    let columns = TimelineColumns {
        start: 1,
        end: 7,
        playhead: 4,
        active_handle: Some(SelectionHandle::Start),
    };
    let fades = FadeColumns {
        fade_in_end: Some(3),
        fade_out_start: Some(5),
        fade_in_active: false,
        fade_out_active: false,
    };
    let lines = video_waveform_lines(
        &[0.1, 0.25, 0.5, 0.75, 1.0, 0.75, 0.5, 0.25],
        8,
        3,
        columns,
        fades,
    );
    let center = &lines[1].spans;

    assert_eq!(lines.len(), 3);
    assert_eq!(center.len(), 8);
    assert_eq!(center[0].style.fg, Some(BORDER));
    assert_eq!(center[2].content.as_ref(), "╱");
    assert_eq!(center[2].style.fg, Some(CYAN));
    assert_eq!(center[3].style.fg, Some(GREEN));
    assert_eq!(center[4].content.as_ref(), "│");
    assert_eq!(center[4].style.fg, Some(Color::White));
    assert_eq!(center[5].content.as_ref(), "╲");
    assert_eq!(center[5].style.fg, Some(PURPLE));
}

#[test]
fn video_editor_renders_loaded_waveform_fades_and_tool_summary() {
    let mut app = video_app();
    let _commands = attach_video_editor(&mut app);
    let editor = app.editor.as_mut().unwrap();
    editor.waveform = Some(EditorWaveform::Ready(vec![1.0; 98]));
    editor.position = Duration::from_secs(45);
    editor.edit_settings.audio_fade_in = Duration::from_secs(30);
    editor.edit_settings.audio_fade_out = Duration::from_secs(20);
    editor.edit_settings.video_fade_in = Duration::from_secs(15);
    editor.edit_settings.video_fade_out = Duration::from_secs(10);
    editor.edit_settings.crop_left = 100;
    editor.edit_settings.crop_right = 100;
    editor.active_tool = EditorTool::AudioFadeIn;

    let backend = TestBackend::new(100, 30);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| app.render(frame)).unwrap();
    let buffer = terminal.backend().buffer();

    let waveform_title = buffer_row(buffer, 19);
    assert!(waveform_title.contains("WAVEFORM"));
    assert!(!waveform_title.contains("not loaded"));
    assert_eq!(buffer[(10, 20)].symbol(), "█");
    assert_eq!(buffer[(33, 21)].symbol(), "╱");
    assert_eq!(buffer[(33, 21)].fg, AMBER);
    assert_eq!(buffer[(77, 21)].symbol(), "╲");
    assert_eq!(buffer[(77, 21)].fg, PURPLE);
    assert_eq!(buffer[(17, 16)].symbol(), "╱");
    assert_eq!(buffer[(17, 16)].fg, CYAN);
    assert_eq!(buffer[(88, 16)].symbol(), "╲");
    assert_eq!(buffer[(88, 16)].fg, PURPLE);
    assert_eq!(buffer[(1, 20)].fg, CYAN);

    let tool = buffer_row(buffer, 25);
    assert!(tool.contains("T"));
    assert!(tool.contains("V"));
    assert!(tool.contains("A"));
    assert!(tool.contains("C"));
    assert!(tool.contains("AUDIO FADE IN"));
    assert!(tool.contains("30s"));
    assert!(tool.contains("2360x1440"));
}

#[test]
fn compact_video_footer_keeps_every_action_visible() {
    let mut app = video_app();
    let _commands = attach_video_editor(&mut app);
    let backend = TestBackend::new(60, 30);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| app.render(frame)).unwrap();
    let first = buffer_row(terminal.backend().buffer(), 28);
    let second = buffer_row(terminal.backend().buffer(), 29);
    assert!(first.contains("Tab/⇧Tab tools"));
    assert!(first.contains("T/V/A/C group"));
    assert!(first.contains("Space play"));
    assert!(second.contains("Bksp start"));
    assert!(second.contains("R all"));
    assert!(second.contains("Enter apply"));
    assert!(second.contains("Esc original"));
    assert!(second.contains("? help"));
}

#[test]
fn video_trim_loader_reports_plain_english_progress() {
    let mut app = video_app();
    let _commands = attach_video_editor(&mut app);
    let progress = Arc::new(Mutex::new(TrimProgress::Processing {
        completed_frames: 45,
        total_frames: 100,
    }));
    let worker = thread::spawn(|| Err(anyhow::anyhow!("test worker")));
    app.editor.as_mut().unwrap().trim_job = Some(VideoTrimJob {
        worker,
        progress,
        started: Instant::now(),
    });
    let backend = TestBackend::new(100, 30);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| app.render(frame)).unwrap();
    let content = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(content.contains("SAVING YOUR VIDEO"));
    assert!(content.contains("Processing the selected video"));
    assert!(content.contains("45%"));
    assert!(content.contains("original stays untouched"));

    let job = app.editor.as_mut().unwrap().trim_job.take().unwrap();
    let _ = job.worker.join();
}

#[test]
fn loader_animation_and_progress_are_deterministic_and_bounded() {
    assert_eq!(spinner_frame(Duration::ZERO), "⠋");
    assert_eq!(spinner_frame(Duration::from_millis(80)), "⠙");
    assert_eq!(spinner_frame(Duration::from_millis(800)), "⠋");
    assert_eq!(progress_ratio(1, 4), 0.25);
    assert_eq!(progress_ratio(5, 4), 1.0);
    assert_eq!(progress_ratio(1, 0), 0.0);
}

#[test]
fn video_editor_renders_frame_preview_and_clip_range() {
    let mut app = video_app();
    let _commands = attach_video_editor(&mut app);
    let backend = TestBackend::new(100, 30);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| app.render(frame)).unwrap();
    let content = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(content.contains("VIDEO CLIP"));
    assert!(content.contains("FRAME PREVIEW"));
    assert!(content.contains("CLIP RANGE"));
    assert!(content.contains("WAVEFORM"));
    assert!(content.contains("apply edits"));
}
