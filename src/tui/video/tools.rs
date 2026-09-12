//! Focus and navigation for the integrated video editor's adjustment tools.

use crate::clip::SelectionHandle;

/// Identifies the editor value currently owned by the arrow keys.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum EditorTool {
    RangeStart,
    RangeEnd,
    VideoFadeIn,
    VideoFadeOut,
    AudioFadeIn,
    AudioFadeOut,
    CropLeft,
    CropRight,
    CropTop,
    CropBottom,
}

impl EditorTool {
    const ALL: [Self; 10] = [
        Self::RangeStart,
        Self::RangeEnd,
        Self::VideoFadeIn,
        Self::VideoFadeOut,
        Self::AudioFadeIn,
        Self::AudioFadeOut,
        Self::CropLeft,
        Self::CropRight,
        Self::CropTop,
        Self::CropBottom,
    ];

    /// Selects the value after this one in the editor's visual tool order.
    pub(super) fn next(self) -> Self {
        self.offset(1)
    }

    /// Selects the value before this one in the editor's visual tool order.
    pub(super) fn previous(self) -> Self {
        self.offset(Self::ALL.len() - 1)
    }

    /// Selects or advances through the two range handles.
    pub(super) const fn cycle_range(self) -> Self {
        match self {
            Self::RangeStart => Self::RangeEnd,
            _ => Self::RangeStart,
        }
    }

    /// Selects or advances through video fade-in and fade-out.
    pub(super) const fn cycle_video_fade(self) -> Self {
        match self {
            Self::VideoFadeIn => Self::VideoFadeOut,
            _ => Self::VideoFadeIn,
        }
    }

    /// Selects or advances through audio fade-in and fade-out.
    pub(super) const fn cycle_audio_fade(self) -> Self {
        match self {
            Self::AudioFadeIn => Self::AudioFadeOut,
            _ => Self::AudioFadeIn,
        }
    }

    /// Selects or advances through the four crop edges.
    pub(super) const fn cycle_crop(self) -> Self {
        match self {
            Self::CropLeft => Self::CropRight,
            Self::CropRight => Self::CropTop,
            Self::CropTop => Self::CropBottom,
            _ => Self::CropLeft,
        }
    }

    /// Returns the range handle represented by this tool, when applicable.
    pub(super) const fn range_handle(self) -> Option<SelectionHandle> {
        match self {
            Self::RangeStart => Some(SelectionHandle::Start),
            Self::RangeEnd => Some(SelectionHandle::End),
            _ => None,
        }
    }

    /// Returns a compact user-facing name for headers and the tool strip.
    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::RangeStart => "RANGE START",
            Self::RangeEnd => "RANGE END",
            Self::VideoFadeIn => "VIDEO FADE IN",
            Self::VideoFadeOut => "VIDEO FADE OUT",
            Self::AudioFadeIn => "AUDIO FADE IN",
            Self::AudioFadeOut => "AUDIO FADE OUT",
            Self::CropLeft => "CROP LEFT",
            Self::CropRight => "CROP RIGHT",
            Self::CropTop => "CROP TOP",
            Self::CropBottom => "CROP BOTTOM",
        }
    }

    /// Reports whether this tool changes a crop edge.
    pub(super) const fn is_crop(self) -> bool {
        matches!(
            self,
            Self::CropLeft | Self::CropRight | Self::CropTop | Self::CropBottom
        )
    }

    fn offset(self, amount: usize) -> Self {
        let index = Self::ALL
            .iter()
            .position(|candidate| *candidate == self)
            .expect("every editor tool is in the navigation order");
        Self::ALL[(index + amount) % Self::ALL.len()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_order_visits_every_editor_value_in_display_order() {
        let mut tool = EditorTool::RangeStart;
        for expected in EditorTool::ALL.iter().copied().skip(1) {
            tool = tool.next();
            assert_eq!(tool, expected);
        }
        assert_eq!(tool.next(), EditorTool::RangeStart);
        assert_eq!(EditorTool::RangeStart.previous(), EditorTool::CropBottom);
    }

    #[test]
    fn direct_group_keys_cycle_only_their_handles() {
        assert_eq!(EditorTool::RangeStart.cycle_range(), EditorTool::RangeEnd);
        assert_eq!(
            EditorTool::AudioFadeIn.cycle_video_fade(),
            EditorTool::VideoFadeIn
        );
        assert_eq!(
            EditorTool::AudioFadeIn.cycle_audio_fade(),
            EditorTool::AudioFadeOut
        );
        assert_eq!(EditorTool::CropBottom.cycle_crop(), EditorTool::CropLeft);
    }
}
