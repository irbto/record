//! The desktop dashboard of `record`: sound, video, and screenshots in one window.
//!
//! The dashboard is a second user interface beside the terminal program. It
//! draws with egui and drives the engines of `record-core`.

#![warn(missing_docs)]

/// The dashboard state and its drawing code.
pub mod app;
/// The boundary between the dashboard and the capture engines.
pub mod backend;
/// The engines of `record-core` behind the dashboard boundary.
#[cfg(windows)]
pub mod engines;
/// The dashboard look.
pub mod theme;
