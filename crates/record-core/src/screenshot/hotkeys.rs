//! The global hotkeys as text and as Windows key codes.
//!
//! A hotkey is a chord: any of Ctrl, Alt, Shift, and Win, and one key. The
//! settings file and the interfaces hold a chord as text such as
//! `Ctrl+Shift+F8`. The capture thread needs the modifier bits and the
//! virtual key code that `RegisterHotKey` takes. Nothing here calls Windows.

/// Modifier bit of the Alt key, as `RegisterHotKey` defines it.
pub const MODIFIER_ALT: u32 = 1;
/// Modifier bit of the Ctrl key.
pub const MODIFIER_CONTROL: u32 = 2;
/// Modifier bit of the Shift key.
pub const MODIFIER_SHIFT: u32 = 4;
/// Modifier bit of the Windows key.
pub const MODIFIER_WIN: u32 = 8;

/// Virtual key code of the Print Screen key.
const KEY_PRINT_SCREEN: u16 = 0x2c;
/// Virtual key code of F1. The other function keys follow it.
const KEY_F1: u16 = 0x70;
/// Virtual key code of the 0 key of the number pad. The other digits follow.
const KEY_NUMPAD0: u16 = 0x60;

/// Every key that has a name, with the names that the parser accepts.
///
/// The first name is the one that [`Chord::to_text`] writes. The flag marks
/// a key that types or edits text. Such a key needs Ctrl, Alt, or Win.
const NAMED_KEYS: [(u16, bool, &[&str]); 34] = [
    (
        KEY_PRINT_SCREEN,
        false,
        &["PrtSc", "PrintScreen", "PrtScn", "Print", "Snapshot"],
    ),
    (0x20, true, &["Space", "Spacebar"]),
    (0x0d, true, &["Enter", "Return"]),
    (0x09, true, &["Tab"]),
    (0x1b, true, &["Esc", "Escape"]),
    (0x08, true, &["Backspace"]),
    (0x2d, false, &["Insert", "Ins"]),
    (0x2e, true, &["Delete", "Del"]),
    (0x24, true, &["Home"]),
    (0x23, true, &["End"]),
    (0x21, true, &["PageUp", "PgUp"]),
    (0x22, true, &["PageDown", "PgDn"]),
    (0x25, true, &["Left", "ArrowLeft"]),
    (0x26, true, &["Up", "ArrowUp"]),
    (0x27, true, &["Right", "ArrowRight"]),
    (0x28, true, &["Down", "ArrowDown"]),
    (0x13, false, &["Pause", "Break"]),
    (0x91, false, &["ScrollLock"]),
    (0x90, false, &["NumLock"]),
    (0x14, false, &["CapsLock"]),
    (0xbd, true, &["Minus", "-"]),
    (0xbb, true, &["Plus", "Equal", "="]),
    (0xbc, true, &["Comma", ","]),
    (0xbe, true, &["Period", "."]),
    (0xbf, true, &["Slash", "/"]),
    (0xba, true, &["Semicolon", ";"]),
    (0xde, true, &["Quote", "'"]),
    (0xdb, true, &["BracketLeft", "["]),
    (0xdd, true, &["BracketRight", "]"]),
    (0xdc, true, &["Backslash", "\\"]),
    (0xc0, true, &["Backquote", "`"]),
    (0x6a, false, &["NumpadMultiply"]),
    (0x6b, false, &["NumpadAdd"]),
    (0x6d, false, &["NumpadSubtract"]),
];

/// One key with its modifiers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Chord {
    /// The Ctrl key is part of the chord.
    pub ctrl: bool,
    /// The Alt key is part of the chord.
    pub alt: bool,
    /// The Shift key is part of the chord.
    pub shift: bool,
    /// The Windows key is part of the chord.
    pub win: bool,
    /// Windows virtual key code of the key.
    pub key: u16,
}

/// Finds the virtual key code of one key name and whether the key types text.
fn key_code(name: &str) -> Option<(u16, bool)> {
    let upper = name.to_ascii_uppercase();
    if let [byte] = upper.as_bytes()
        && byte.is_ascii_alphanumeric()
    {
        // Letters and digits use their ASCII code.
        return Some((u16::from(*byte), true));
    }
    if let Some(number) = upper.strip_prefix('F')
        && let Ok(number) = number.parse::<u16>()
        && (1..=24).contains(&number)
    {
        return Some((KEY_F1 + number - 1, false));
    }
    if let Some(digit) = upper.strip_prefix("NUMPAD")
        && let Ok(digit) = digit.parse::<u16>()
        && digit <= 9
    {
        return Some((KEY_NUMPAD0 + digit, false));
    }
    NAMED_KEYS
        .iter()
        .find(|(_, _, names)| names.iter().any(|known| known.eq_ignore_ascii_case(name)))
        .map(|(code, typing, _)| (*code, *typing))
}

/// Returns the name of one virtual key code, when the key has one.
fn key_name(code: u16) -> Option<String> {
    match code {
        0x30..=0x39 | 0x41..=0x5a => Some(char::from(code as u8).to_string()),
        0x70..=0x87 => Some(format!("F{}", code - KEY_F1 + 1)),
        0x60..=0x69 => Some(format!("Numpad{}", code - KEY_NUMPAD0)),
        _ => NAMED_KEYS
            .iter()
            .find(|(known, _, _)| *known == code)
            .map(|(_, _, names)| names[0].to_owned()),
    }
}

impl Chord {
    /// Builds a chord of one key without modifiers.
    #[must_use]
    pub const fn plain(key: u16) -> Self {
        Self {
            ctrl: false,
            alt: false,
            shift: false,
            win: false,
            key,
        }
    }

    /// Parses text such as `Ctrl+Shift+F8`.
    ///
    /// The parts are separated by `+`. Upper and lower case are equal. The
    /// order of the modifiers is free. The text needs exactly one key.
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut chord = Self::plain(0);
        let mut key = None;
        if text.trim().is_empty() {
            return Err("Enter a key such as Ctrl+Shift+F8".to_owned());
        }
        for part in text.split('+').map(str::trim) {
            if part.is_empty() {
                return Err(format!(
                    "{text} is not a hotkey. Write the plus key as Plus."
                ));
            }
            match part.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => chord.ctrl = true,
                "alt" => chord.alt = true,
                "shift" => chord.shift = true,
                "win" | "windows" | "meta" | "super" | "cmd" => chord.win = true,
                _ => {
                    let found =
                        key_code(part).ok_or_else(|| format!("{part} is not a key name"))?;
                    if key.replace(found).is_some() {
                        return Err(format!("{text} has more than one key"));
                    }
                }
            }
        }
        let (code, typing) = key.ok_or_else(|| format!("{text} has no key, only modifiers"))?;
        chord.key = code;
        if typing && !(chord.ctrl || chord.alt || chord.win) {
            return Err(format!(
                "{} types text. Add Ctrl, Alt, or Win.",
                chord.to_text()
            ));
        }
        Ok(chord)
    }

    /// Formats the chord with the modifiers in the order Ctrl, Alt, Shift,
    /// Win, and then the key.
    #[must_use]
    pub fn to_text(&self) -> String {
        let mut parts: Vec<String> = [
            (self.ctrl, "Ctrl"),
            (self.alt, "Alt"),
            (self.shift, "Shift"),
            (self.win, "Win"),
        ]
        .iter()
        .filter(|(held, _)| *held)
        .map(|(_, name)| (*name).to_owned())
        .collect();
        parts.push(key_name(self.key).unwrap_or_else(|| format!("Key{}", self.key)));
        parts.join("+")
    }

    /// Returns the modifier bits that `RegisterHotKey` takes.
    #[must_use]
    pub const fn modifiers(&self) -> u32 {
        (if self.alt { MODIFIER_ALT } else { 0 })
            | (if self.ctrl { MODIFIER_CONTROL } else { 0 })
            | (if self.shift { MODIFIER_SHIFT } else { 0 })
            | (if self.win { MODIFIER_WIN } else { 0 })
    }
}

/// One of the five actions that a global hotkey starts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HotkeyId {
    /// Screenshot of a region with the annotation overlay.
    Region,
    /// Screenshot of every monitor.
    Fullscreen,
    /// Screenshot of the active window.
    Window,
    /// Video recording of a region. The same key saves it.
    VideoRegion,
    /// Video recording of one monitor. The same key saves it.
    VideoFullscreen,
}

impl HotkeyId {
    /// Every hotkey in the order of the settings and the reports.
    pub const ALL: [Self; 5] = [
        Self::Region,
        Self::Fullscreen,
        Self::Window,
        Self::VideoRegion,
        Self::VideoFullscreen,
    ];

    /// Returns the position of this hotkey in [`Self::ALL`].
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Region => 0,
            Self::Fullscreen => 1,
            Self::Window => 2,
            Self::VideoRegion => 3,
            Self::VideoFullscreen => 4,
        }
    }

    /// Returns the name that the desktop interface uses.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Region => "region",
            Self::Fullscreen => "fullscreen",
            Self::Window => "window",
            Self::VideoRegion => "videoRegion",
            Self::VideoFullscreen => "videoFullscreen",
        }
    }

    /// Returns the name of this hotkey in the settings file.
    #[must_use]
    pub const fn setting(self) -> &'static str {
        match self {
            Self::Region => "hotkey_region",
            Self::Fullscreen => "hotkey_fullscreen",
            Self::Window => "hotkey_window",
            Self::VideoRegion => "hotkey_video_region",
            Self::VideoFullscreen => "hotkey_video_fullscreen",
        }
    }

    /// Returns the text that a settings screen shows for this hotkey.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Region => "Screenshot region",
            Self::Fullscreen => "Screenshot fullscreen",
            Self::Window => "Screenshot window",
            Self::VideoRegion => "Record region",
            Self::VideoFullscreen => "Record fullscreen",
        }
    }

    /// Finds a hotkey by the name that the desktop interface uses.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|id| id.name() == name)
    }

    /// Reports whether this hotkey starts a video recording.
    #[must_use]
    pub const fn is_video(self) -> bool {
        matches!(self, Self::VideoRegion | Self::VideoFullscreen)
    }

    /// Returns the chord of this hotkey before the user changes it.
    #[must_use]
    pub const fn default_chord(self) -> Chord {
        let plain = Chord::plain(KEY_PRINT_SCREEN);
        match self {
            Self::Region => Chord {
                ctrl: true,
                ..plain
            },
            Self::Fullscreen => plain,
            Self::Window => Chord { alt: true, ..plain },
            Self::VideoRegion => Chord {
                ctrl: true,
                shift: true,
                ..Chord::plain(KEY_F1 + 7)
            },
            Self::VideoFullscreen => Chord {
                ctrl: true,
                shift: true,
                ..Chord::plain(KEY_F1 + 8)
            },
        }
    }
}

/// The chord of one hotkey and whether the user wants it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Binding {
    /// The keys.
    pub chord: Chord,
    /// The user wants this hotkey.
    pub enabled: bool,
}

/// The bindings of all hotkeys in the order of [`HotkeyId::ALL`].
pub type Bindings = [Binding; 5];

/// Returns the bindings before the user changes them.
#[must_use]
pub fn default_bindings() -> Bindings {
    HotkeyId::ALL.map(|id| Binding {
        chord: id.default_chord(),
        enabled: true,
    })
}

/// Finds another enabled hotkey that already uses a chord.
#[must_use]
pub fn conflict(bindings: &Bindings, id: HotkeyId, chord: Chord) -> Option<HotkeyId> {
    HotkeyId::ALL.into_iter().find(|other| {
        let binding = bindings[other.index()];
        *other != id && binding.enabled && binding.chord == chord
    })
}

/// The state of one hotkey for a settings screen.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HotkeyState {
    /// The action.
    pub id: HotkeyId,
    /// The chord as text.
    pub keys: String,
    /// The user wants this hotkey.
    pub enabled: bool,
    /// Windows gave the chord to this program.
    pub registered: bool,
}

/// Joins the bindings and the registration results into one report.
#[must_use]
pub fn hotkey_states(bindings: &Bindings, registered: [bool; 5]) -> Vec<HotkeyState> {
    HotkeyId::ALL
        .into_iter()
        .map(|id| {
            let binding = bindings[id.index()];
            HotkeyState {
                id,
                keys: binding.chord.to_text(),
                enabled: binding.enabled,
                registered: binding.enabled && registered[id.index()],
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_chords_have_the_documented_text() {
        let texts: Vec<String> = default_bindings()
            .iter()
            .map(|binding| binding.chord.to_text())
            .collect();
        assert_eq!(
            texts,
            [
                "Ctrl+PrtSc",
                "PrtSc",
                "Alt+PrtSc",
                "Ctrl+Shift+F8",
                "Ctrl+Shift+F9"
            ]
        );
        assert_eq!(HotkeyId::Region.default_chord().modifiers(), 2);
        assert_eq!(HotkeyId::Region.default_chord().key, 0x2c);
        assert_eq!(HotkeyId::VideoRegion.default_chord().key, 0x77);
    }

    #[test]
    fn text_parses_in_any_order_and_case_and_formats_in_one_order() {
        for (text, expected) in [
            ("shift+ctrl+f8", "Ctrl+Shift+F8"),
            (" Win + Alt + r ", "Alt+Win+R"),
            ("Control+PrintScreen", "Ctrl+PrtSc"),
            ("Meta+ArrowLeft", "Win+Left"),
            ("ctrl+alt+shift+win+0", "Ctrl+Alt+Shift+Win+0"),
            ("F24", "F24"),
            ("Ctrl+Plus", "Ctrl+Plus"),
            ("Ctrl+=", "Ctrl+Plus"),
            ("Alt+Numpad7", "Alt+Numpad7"),
            ("Pause", "Pause"),
            ("Ctrl+Escape", "Ctrl+Esc"),
            ("alt+`", "Alt+Backquote"),
        ] {
            let chord = Chord::parse(text).unwrap();
            assert_eq!(chord.to_text(), expected, "{text}");
            assert_eq!(Chord::parse(&chord.to_text()), Ok(chord), "{text}");
        }
        let chord = Chord::parse("Ctrl+Alt+Shift+Win+A").unwrap();
        assert_eq!(chord.modifiers(), 15);
        assert_eq!(chord.key, 0x41);
        assert_eq!(Chord::parse("F1").unwrap().key, 0x70);
        assert_eq!(Chord::parse("Ctrl+9").unwrap().key, 0x39);
    }

    #[test]
    fn every_named_key_round_trips() {
        for (code, _, names) in NAMED_KEYS {
            for name in names {
                let chord = Chord::parse(&format!("Ctrl+{name}"));
                if *name == "+" {
                    continue;
                }
                assert_eq!(chord.map(|chord| chord.key), Ok(code), "{name}");
            }
            assert_eq!(key_name(code).as_deref(), Some(names[0]));
        }
        for number in 1..=24_u16 {
            let chord = Chord::parse(&format!("F{number}")).unwrap();
            assert_eq!(chord.to_text(), format!("F{number}"));
        }
    }

    #[test]
    fn bad_text_gives_a_reason() {
        for text in [
            "",
            "Ctrl",
            "Ctrl+Shift",
            "Ctrl+A+B",
            "Ctrl++",
            "F25",
            "F0",
            "Ctrl+Hyper",
            "Numpad10",
            "+",
        ] {
            assert!(Chord::parse(text).is_err(), "{text} should fail");
        }
        // A key that types text needs a modifier other than Shift.
        for text in ["A", "Shift+A", "5", "Space", "Shift+Enter", "Delete"] {
            let error = Chord::parse(text).unwrap_err();
            assert!(error.contains("Add Ctrl"), "{text}: {error}");
        }
        assert!(Chord::parse("Shift+F5").is_ok());
        assert!(Chord::parse("Insert").is_ok());
    }

    #[test]
    fn names_find_their_hotkey() {
        for id in HotkeyId::ALL {
            assert_eq!(HotkeyId::parse(id.name()), Some(id));
            assert_eq!(HotkeyId::ALL[id.index()], id);
        }
        assert_eq!(HotkeyId::parse("video"), None);
        assert!(HotkeyId::VideoFullscreen.is_video());
        assert!(!HotkeyId::Window.is_video());
    }

    #[test]
    fn a_report_shows_a_disabled_hotkey_as_not_registered() {
        let mut bindings = default_bindings();
        bindings[1].enabled = false;
        let states = hotkey_states(&bindings, [true, true, false, true, true]);
        let flags: Vec<(bool, bool)> = states
            .iter()
            .map(|state| (state.enabled, state.registered))
            .collect();
        assert_eq!(
            flags,
            [
                (true, true),
                (false, false),
                (true, false),
                (true, true),
                (true, true)
            ]
        );
        assert_eq!(states[3].keys, "Ctrl+Shift+F8");
    }

    #[test]
    fn a_chord_conflicts_only_with_another_enabled_hotkey() {
        let mut bindings = default_bindings();
        let print = HotkeyId::Fullscreen.default_chord();
        assert_eq!(
            conflict(&bindings, HotkeyId::Window, print),
            Some(HotkeyId::Fullscreen)
        );
        assert_eq!(conflict(&bindings, HotkeyId::Fullscreen, print), None);
        bindings[1].enabled = false;
        assert_eq!(conflict(&bindings, HotkeyId::Window, print), None);
    }
}
