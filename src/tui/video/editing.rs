//! Keyboard adjustment behavior for range, fade, and crop tools.

use std::time::Duration;

use crossterm::event::{KeyCode, KeyModifiers};

use crate::raster::{CropEdge, RasterEditOverlay};

use super::{
    VideoApp, VideoEditor, audio_preview::pause_audio_preview, frame_time, selection_end_time,
    selection_start_time, tools::EditorTool,
};

impl VideoEditor {
    /// Returns pixel-independent crop guides and the current video-fade gain.
    pub(super) fn raster_edit_overlay(&self) -> Option<RasterEditOverlay> {
        let settings = self.edit_settings;
        let has_crop = settings.crop_left != 0
            || settings.crop_right != 0
            || settings.crop_top != 0
            || settings.crop_bottom != 0;
        let show_crop = has_crop || self.active_tool.is_crop();
        let video_gain = video_preview_gain(self);
        if !show_crop && video_gain >= 1.0 {
            return None;
        }
        let width = self.summary.width.max(1) as f32;
        let height = self.summary.height.max(1) as f32;
        let active = match self.active_tool {
            EditorTool::CropLeft => Some(CropEdge::Left),
            EditorTool::CropRight => Some(CropEdge::Right),
            EditorTool::CropTop => Some(CropEdge::Top),
            EditorTool::CropBottom => Some(CropEdge::Bottom),
            _ => None,
        };
        Some(RasterEditOverlay {
            left: settings.crop_left as f32 / width,
            right: settings.crop_right as f32 / width,
            top: settings.crop_top as f32 / height,
            bottom: settings.crop_bottom as f32 / height,
            active,
            show_crop,
            video_gain,
        })
    }
}

/// Mirrors the exporter's independent linear video envelopes in the preview.
fn video_preview_gain(editor: &VideoEditor) -> f32 {
    let start = selection_start_time(editor);
    let last = selection_end_time(editor).saturating_sub(frame_time(1, editor.summary.fps));
    let fade_in = duration_gain(
        editor.position.saturating_sub(start),
        editor.edit_settings.video_fade_in,
    );
    let fade_out = duration_gain(
        last.saturating_sub(editor.position),
        editor.edit_settings.video_fade_out,
    );
    fade_in.min(fade_out)
}

/// Returns full gain for a disabled fade and a bounded linear ratio otherwise.
fn duration_gain(elapsed: Duration, fade: Duration) -> f32 {
    if fade.is_zero() {
        1.0
    } else {
        (elapsed.as_secs_f64() / fade.as_secs_f64()).clamp(0.0, 1.0) as f32
    }
}

impl VideoApp {
    /// Walks the complete visual tool order with Tab or Shift+Tab.
    pub(super) fn select_adjacent_editor_tool(&mut self, reverse: bool) {
        let Some(editor) = &self.editor else {
            return;
        };
        let tool = if reverse {
            editor.active_tool.previous()
        } else {
            editor.active_tool.next()
        };
        self.activate_editor_tool(tool);
    }

    /// Selects or toggles the range start/end handles.
    pub(super) fn select_range_tool(&mut self) {
        let Some(editor) = &self.editor else {
            return;
        };
        self.activate_editor_tool(editor.active_tool.cycle_range());
    }

    /// Selects or toggles video fade-in/fade-out handles.
    pub(super) fn select_video_fade_tool(&mut self) {
        let Some(editor) = &self.editor else {
            return;
        };
        self.activate_editor_tool(editor.active_tool.cycle_video_fade());
    }

    /// Selects or toggles audio fade-in/fade-out handles.
    pub(super) fn select_audio_fade_tool(&mut self) {
        let Some(editor) = &self.editor else {
            return;
        };
        self.activate_editor_tool(editor.active_tool.cycle_audio_fade());
    }

    /// Selects or advances through left/right/top/bottom crop handles.
    pub(super) fn select_crop_tool(&mut self) {
        let Some(editor) = &self.editor else {
            return;
        };
        self.activate_editor_tool(editor.active_tool.cycle_crop());
    }

    /// Moves the currently selected editor value with one arrow press.
    pub(super) fn adjust_active_editor_tool(&mut self, key: KeyCode, modifiers: KeyModifiers) {
        let Some(editor) = &mut self.editor else {
            return;
        };
        let changed = match editor.active_tool {
            EditorTool::RangeStart | EditorTool::RangeEnd => adjust_range(editor, key, modifiers),
            EditorTool::VideoFadeIn
            | EditorTool::VideoFadeOut
            | EditorTool::AudioFadeIn
            | EditorTool::AudioFadeOut => adjust_fade(editor, key, modifiers),
            EditorTool::CropLeft
            | EditorTool::CropRight
            | EditorTool::CropTop
            | EditorTool::CropBottom => adjust_crop(editor, key, modifiers),
        };
        if !changed {
            return;
        }
        pause_audio_preview(editor);
        editor.playing = false;
        editor.resume_pending = false;
        editor.error = None;
        let seek = editor_tool_time(editor);
        if let Some(time) = seek {
            self.seek_editor(time);
        }
    }

    /// Gives one tool focus and moves the playhead to its timeline handle.
    fn activate_editor_tool(&mut self, tool: EditorTool) {
        let Some(editor) = &mut self.editor else {
            return;
        };
        editor.active_tool = tool;
        sync_range_handle(editor);
        pause_audio_preview(editor);
        editor.playing = false;
        editor.resume_pending = false;
        editor.error = None;
        let seek = editor_tool_time(editor);
        if let Some(time) = seek {
            self.seek_editor(time);
        }
    }
}

/// Ensures legacy range rendering and selection behavior follow the active tool.
fn sync_range_handle(editor: &mut VideoEditor) {
    let Some(handle) = editor.active_tool.range_handle() else {
        return;
    };
    if editor.selection.active_handle() != handle {
        editor.selection.toggle_handle();
    }
}

/// Moves one range boundary by the existing frame/0.1s/1s increments.
fn adjust_range(editor: &mut VideoEditor, key: KeyCode, modifiers: KeyModifiers) -> bool {
    if !matches!(key, KeyCode::Left | KeyCode::Right) {
        return false;
    }
    sync_range_handle(editor);
    let before = (editor.selection.start_frame(), editor.selection.end_frame());
    let rate = i64::from(editor.selection.sample_rate());
    let step = if modifiers.contains(KeyModifiers::SHIFT) {
        rate
    } else if modifiers.contains(KeyModifiers::CONTROL) {
        1
    } else {
        (rate / 10).max(1)
    };
    let direction = if key == KeyCode::Left { -1 } else { 1 };
    editor.selection.nudge(step * direction);
    clamp_fades(editor);
    before != (editor.selection.start_frame(), editor.selection.end_frame())
}

/// Changes one audio or video fade duration and keeps it inside the clip range.
fn adjust_fade(editor: &mut VideoEditor, key: KeyCode, modifiers: KeyModifiers) -> bool {
    let increase = match key {
        KeyCode::Right | KeyCode::Up => true,
        KeyCode::Left | KeyCode::Down => false,
        _ => return false,
    };
    let step = if modifiers.contains(KeyModifiers::SHIFT) {
        Duration::from_secs(1)
    } else if modifiers.contains(KeyModifiers::CONTROL) {
        frame_time(1, editor.summary.fps)
    } else {
        Duration::from_millis(100)
    };
    let maximum = editor.selection.duration();
    let value = match editor.active_tool {
        EditorTool::VideoFadeIn => &mut editor.edit_settings.video_fade_in,
        EditorTool::VideoFadeOut => &mut editor.edit_settings.video_fade_out,
        EditorTool::AudioFadeIn => &mut editor.edit_settings.audio_fade_in,
        EditorTool::AudioFadeOut => &mut editor.edit_settings.audio_fade_out,
        _ => return false,
    };
    let before = *value;
    *value = if increase {
        value.saturating_add(step).min(maximum)
    } else {
        value.saturating_sub(step)
    };
    *value != before
}

/// Moves one crop boundary in its physical screen direction.
fn adjust_crop(editor: &mut VideoEditor, key: KeyCode, modifiers: KeyModifiers) -> bool {
    let increase = match (editor.active_tool, key) {
        (EditorTool::CropLeft, KeyCode::Right)
        | (EditorTool::CropRight, KeyCode::Left)
        | (EditorTool::CropTop, KeyCode::Down)
        | (EditorTool::CropBottom, KeyCode::Up) => true,
        (EditorTool::CropLeft, KeyCode::Left)
        | (EditorTool::CropRight, KeyCode::Right)
        | (EditorTool::CropTop, KeyCode::Up)
        | (EditorTool::CropBottom, KeyCode::Down) => false,
        _ => return false,
    };
    let step = if modifiers.contains(KeyModifiers::SHIFT) {
        20
    } else {
        2
    };
    let settings = &mut editor.edit_settings;
    let (value, maximum) = match editor.active_tool {
        EditorTool::CropLeft => (
            &mut settings.crop_left,
            editor
                .summary
                .width
                .saturating_sub(settings.crop_right)
                .saturating_sub(2),
        ),
        EditorTool::CropRight => (
            &mut settings.crop_right,
            editor
                .summary
                .width
                .saturating_sub(settings.crop_left)
                .saturating_sub(2),
        ),
        EditorTool::CropTop => (
            &mut settings.crop_top,
            editor
                .summary
                .height
                .saturating_sub(settings.crop_bottom)
                .saturating_sub(2),
        ),
        EditorTool::CropBottom => (
            &mut settings.crop_bottom,
            editor
                .summary
                .height
                .saturating_sub(settings.crop_top)
                .saturating_sub(2),
        ),
        _ => return false,
    };
    let before = *value;
    *value = if increase {
        value.saturating_add(step).min(maximum) & !1
    } else {
        value.saturating_sub(step) & !1
    };
    *value != before
}

/// Returns the timeline location represented by the active handle.
fn editor_tool_time(editor: &VideoEditor) -> Option<Duration> {
    let selection_start = selection_start_time(editor);
    let selection_end = selection_end_time(editor);
    let last_frame = selection_end.saturating_sub(frame_time(1, editor.summary.fps));
    match editor.active_tool {
        EditorTool::RangeStart => Some(selection_start),
        EditorTool::RangeEnd => Some(last_frame),
        EditorTool::VideoFadeIn => Some(
            selection_start
                .saturating_add(editor.edit_settings.video_fade_in)
                .min(last_frame),
        ),
        EditorTool::VideoFadeOut => Some(
            selection_end
                .saturating_sub(editor.edit_settings.video_fade_out)
                .min(last_frame),
        ),
        EditorTool::AudioFadeIn => Some(
            selection_start
                .saturating_add(editor.edit_settings.audio_fade_in)
                .min(last_frame),
        ),
        EditorTool::AudioFadeOut => Some(
            selection_end
                .saturating_sub(editor.edit_settings.audio_fade_out)
                .min(last_frame),
        ),
        EditorTool::CropLeft
        | EditorTool::CropRight
        | EditorTool::CropTop
        | EditorTool::CropBottom => None,
    }
}

/// Shrinks existing fades when the selected range becomes shorter.
fn clamp_fades(editor: &mut VideoEditor) {
    let maximum = editor.selection.duration();
    editor.edit_settings.video_fade_in = editor.edit_settings.video_fade_in.min(maximum);
    editor.edit_settings.video_fade_out = editor.edit_settings.video_fade_out.min(maximum);
    editor.edit_settings.audio_fade_in = editor.edit_settings.audio_fade_in.min(maximum);
    editor.edit_settings.audio_fade_out = editor.edit_settings.audio_fade_out.min(maximum);
}
