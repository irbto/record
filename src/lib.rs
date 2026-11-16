//! Terminal components and command-line parsing for the `record` executable.
//!
//! The crate separates Windows audio capture from the terminal interface. The
//! [`audio`] module owns the cross-thread contract. The [`session`] module owns
//! file naming and rotation rules. The [`clip`] module owns non-destructive clip
//! selection before it asks Windows to replace an MP3 safely.
//!
//! The executable starts the audio worker first. Rendering and spectrum work
//! stay off that worker so a slow terminal cannot delay MP3 encoding.
#![warn(missing_docs)]

// The engines live in `record-core`. These names keep the paths that the
// terminal code and the executable already use.
pub use record_core::{audio, clip, screenshot, session, spectrum, video, waveform};

/// Command-line arguments and defaults.
pub mod cli;
pub(crate) mod raster;
/// Ratatui application state and rendering.
pub mod tui;
