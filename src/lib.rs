//! Core types and terminal components for the `record` executable.
//!
//! The crate separates Windows audio capture from the terminal interface. The
//! [`audio`] module owns the cross-thread contract. The [`session`] module owns
//! file naming and rotation rules. The [`clip`] module owns non-destructive clip
//! selection before it asks Windows to replace an MP3 safely.
//!
//! The executable starts the audio worker first. Rendering and spectrum work
//! stay off that worker so a slow terminal cannot delay MP3 encoding.
#![warn(missing_docs)]

/// Audio capture commands, events, and platform entry points.
pub mod audio;
/// Command-line arguments and defaults.
pub mod cli;
/// Named clip selection, preview, and trimming.
pub mod clip;
/// Session directory, part, and clip naming rules.
pub mod session;
/// Bounded FFT data for the spectrum display.
pub mod spectrum;
/// Ratatui application state and rendering.
pub mod tui;
/// Rolling stereo sample history for live scopes and meters.
pub mod waveform;
