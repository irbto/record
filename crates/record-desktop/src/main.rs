//! Starts the `record` desktop program.

// A desktop program has no console window.
#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() {
    record_desktop::run();
}
