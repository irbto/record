//! Windows clip preview through the built-in Media Control Interface.
//!
//! The TUI permits one preview at a time. The process ID is therefore a unique
//! alias inside the MCI command namespace for this process. Playback is async.
//! The TUI polls `status` and closes the device after the selected range stops.

use std::{path::Path, time::Duration};

use anyhow::{Context, Result, bail};
use windows::{
    Win32::Media::Multimedia::{mciGetErrorStringW, mciSendStringW},
    core::PCWSTR,
};

/// Owns one nonblocking MP3 preview in the current process.
pub struct PreviewPlayer {
    alias: String,
    open: bool,
}

impl PreviewPlayer {
    /// Opens an MP3 and plays only the selected time range.
    pub fn start(path: &Path, start: Duration, end: Duration) -> Result<Self> {
        if start >= end {
            bail!("the preview range is empty");
        }
        let absolute = std::path::absolute(path)
            .with_context(|| format!("could not resolve clip path {}", path.display()))?;
        let path_text = absolute.to_string_lossy();
        if path_text.contains('"') {
            bail!("the clip path contains an unsupported quote character");
        }
        let alias = format!("record_preview_{}", std::process::id());
        let mut player = Self { alias, open: false };
        player.command(
            &format!("open \"{path_text}\" type mpegvideo alias {}", player.alias),
            false,
        )?;
        player.open = true;
        player.command(
            &format!("set {} time format milliseconds", player.alias),
            false,
        )?;
        let start_ms = start.as_millis();
        let end_ms = end.as_millis().max(start_ms + 1);
        player.command(
            &format!("play {} from {start_ms} to {end_ms}", player.alias),
            false,
        )?;
        Ok(player)
    }

    /// Reports whether Windows is still playing the selected range.
    pub fn is_playing(&self) -> Result<bool> {
        if !self.open {
            return Ok(false);
        }
        let mode = self.command(&format!("status {} mode", self.alias), true)?;
        Ok(mode.trim().eq_ignore_ascii_case("playing"))
    }

    /// Stops playback and closes the MCI device.
    pub fn stop(&mut self) {
        if self.open {
            let _ = self.command(&format!("stop {}", self.alias), false);
            let _ = self.command(&format!("close {}", self.alias), false);
            self.open = false;
        }
    }

    /// Sends one UTF-16 MCI command and translates a Windows error code.
    fn command(&self, command: &str, wants_output: bool) -> Result<String> {
        let wide = command.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
        let mut output = [0_u16; 128];
        // SAFETY: The command is terminated and remains valid for the call. The
        // optional output slice owns its complete writable capacity.
        let code = unsafe {
            mciSendStringW(
                PCWSTR(wide.as_ptr()),
                wants_output.then_some(output.as_mut_slice()),
                None,
            )
        };
        if code != 0 {
            let mut message = [0_u16; 256];
            // SAFETY: The fixed output array is writable for the complete call.
            let found = unsafe { mciGetErrorStringW(code, &mut message) }.as_bool();
            let text = if found {
                String::from_utf16_lossy(
                    &message[..message
                        .iter()
                        .position(|unit| *unit == 0)
                        .unwrap_or(message.len())],
                )
            } else {
                format!("Windows multimedia error {code}")
            };
            bail!("{text}");
        }
        let length = output
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(output.len());
        Ok(String::from_utf16_lossy(&output[..length]))
    }
}

impl Drop for PreviewPlayer {
    fn drop(&mut self) {
        self.stop();
    }
}
