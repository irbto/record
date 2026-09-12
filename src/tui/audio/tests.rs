use super::super::utils::{compact_path, format_clock};
use super::render::{format_clip_time, waveform_points};
use super::*;
use std::path::Path;

use crossbeam_channel::bounded;
use ratatui::{Terminal, backend::TestBackend};

fn session_app() -> (App, Receiver<AudioCommand>) {
    let (commands, receiver) = bounded(8);
    let app = App::new(
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
        commands,
        OutputTarget::Session {
            directory: PathBuf::from("recording-session"),
            segment_duration: Duration::from_secs(600),
        },
    );
    (app, receiver)
}

fn press(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    app.handle_event(Event::Key(KeyEvent::new(code, modifiers)));
}

fn saved_file(name: &str, kind: SavedFileKind) -> SavedFile {
    SavedFile {
        path: PathBuf::from(name),
        kind,
        sample_rate: 48_000,
        bitrate: 320_000,
        frames: 48_000,
        edit_source: None,
    }
}

#[test]
fn split_is_the_default_view() {
    let (app, _commands) = session_app();
    assert!(matches!(app.view, ViewMode::Split));
    assert!(app.spectrum.is_none());
}

#[test]
fn view_key_cycles_all_three_modes() {
    let (mut app, _commands) = session_app();
    press(&mut app, KeyCode::Char('w'), KeyModifiers::NONE);
    assert!(matches!(app.view, ViewMode::Waveform));
    press(&mut app, KeyCode::Char('w'), KeyModifiers::NONE);
    assert!(matches!(app.view, ViewMode::Spectrum));
    assert!(app.spectrum.is_some());
    press(&mut app, KeyCode::Char('w'), KeyModifiers::NONE);
    assert!(matches!(app.view, ViewMode::Split));
}

#[test]
fn clip_prompt_validates_and_sends_a_trimmed_name() {
    let (mut app, commands) = session_app();
    app.state = CaptureState::Recording;
    press(&mut app, KeyCode::Char('c'), KeyModifiers::NONE);
    for character in "  demo clip.mp3  ".chars() {
        press(&mut app, KeyCode::Char(character), KeyModifiers::NONE);
    }
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        commands.try_recv().unwrap(),
        AudioCommand::SaveClip("demo clip".to_owned())
    );
    assert!(app.clip_prompt.is_none());
}

#[test]
fn invalid_clip_name_keeps_the_prompt_open() {
    let (mut app, commands) = session_app();
    app.state = CaptureState::Recording;
    press(&mut app, KeyCode::Char('c'), KeyModifiers::NONE);
    for character in "NUL".chars() {
        press(&mut app, KeyCode::Char(character), KeyModifiers::NONE);
    }
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(commands.try_recv().is_err());
    assert!(
        app.clip_prompt
            .as_ref()
            .and_then(|prompt| prompt.error.as_ref())
            .is_some()
    );
}

#[test]
fn explicit_output_disables_the_clip_prompt() {
    let (commands, _receiver) = bounded(1);
    let mut app = App::new(
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
        commands,
        OutputTarget::SingleFile {
            path: PathBuf::from("take.mp3"),
            replace: false,
        },
    );
    app.state = CaptureState::Recording;
    press(&mut app, KeyCode::Char('c'), KeyModifiers::NONE);
    assert!(app.clip_prompt.is_none());
}

#[test]
fn control_shift_c_stops_capture_from_any_modal() {
    let (mut app, _commands) = session_app();
    app.state = CaptureState::Recording;
    app.clip_prompt = Some(ClipPrompt::default());
    press(
        &mut app,
        KeyCode::Char('C'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    );
    assert!(app.stop.load(Ordering::Relaxed));
    assert!(matches!(app.state, CaptureState::Finalizing));
}

#[test]
fn audio_events_update_state_samples_and_saved_selection() {
    let (mut app, _commands) = session_app();
    let (sender, receiver) = bounded(8);
    sender
        .send(AudioEvent::Started {
            sample_rate: 44_100,
            bitrate: 192_000,
            channels: 2,
        })
        .unwrap();
    sender
        .send(AudioEvent::Samples {
            left: vec![0.5],
            right: vec![-0.25],
            encoded_frames: 44_100,
        })
        .unwrap();
    sender
        .send(AudioEvent::Saved(saved_file(
            "part-001.mp3",
            SavedFileKind::Part,
        )))
        .unwrap();
    sender
        .send(AudioEvent::Notice("backend notice".to_owned()))
        .unwrap();
    app.drain_audio_events(&receiver);
    assert!(matches!(app.state, CaptureState::Recording));
    assert_eq!(app.sample_rate, 44_100);
    assert_eq!(app.bitrate, 192_000);
    assert_eq!(app.encoded_frames, 44_100);
    assert_eq!(app.saved.len(), 1);
    assert_eq!(app.selected_save, Some(0));
    assert_eq!(app.notice.as_deref(), Some("backend notice"));
    assert!(app.spectrum.is_some());
}

#[test]
fn session_view_renders_in_a_standard_terminal() {
    let (mut app, _commands) = session_app();
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
    assert!(content.contains("SPLIT"));
    assert!(content.contains("SAVES"));
    assert!(content.contains("Ctrl+C"));
}

#[test]
fn compacts_from_the_front_to_keep_the_filename() {
    let result = compact_path(Path::new("C:/very/long/path/recording.mp3"), 18);
    assert!(result.starts_with("..."));
    assert!(result.ends_with("recording.mp3"));
}

#[test]
fn compact_path_never_exceeds_tiny_limits() {
    for maximum in 0..=3 {
        assert!(compact_path(Path::new("recording.mp3"), maximum).len() <= maximum);
    }
}

#[test]
fn clip_time_formats_minutes_hours_and_zero_rate() {
    assert_eq!(format_clip_time(0, 0), "00:00.000");
    assert_eq!(format_clip_time(90_061, 1_000), "01:30.061");
    assert_eq!(format_clip_time(3_661_001, 1_000), "01:01:01.001");
}

#[test]
fn waveform_downsamples_to_a_bounded_size() {
    let samples = (0..10_000)
        .map(|index| index as f32)
        .collect::<VecDeque<_>>();
    assert!(waveform_points(&samples, 100).len() <= 100);
}

#[test]
fn empty_waveform_has_a_visible_zero_line() {
    assert_eq!(
        waveform_points(&VecDeque::new(), 0),
        vec![(0.0, 0.0), (1.0, 0.0)]
    );
}

#[test]
fn clock_formats_hours_minutes_and_seconds() {
    assert_eq!(format_clock(Duration::ZERO), "00:00:00");
    assert_eq!(format_clock(Duration::from_secs(61)), "00:01:01");
    assert_eq!(format_clock(Duration::from_secs(3_661)), "01:01:01");
}
