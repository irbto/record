//! Capture engines of `record`: system audio, screen video, and screenshots.
//!
//! This crate has no user interface. The terminal program and the desktop
//! program both drive these engines and draw their own screens.

#![warn(missing_docs)]

/// WASAPI loopback capture and native MP3 encoding.
pub mod audio;
/// Saved clips and their conversions.
pub mod clip;
/// HDR-aware screen capture with an on-screen annotation overlay.
pub mod screenshot;
/// Recording session folders and output file names.
pub mod session;
/// Spectrum analysis for level displays.
pub mod spectrum;
/// Colors shared by every user interface.
pub mod theme;
/// DXGI screen capture to H.264/AAC MP4.
pub mod video;
/// Waveform summaries for level displays.
pub mod waveform;
