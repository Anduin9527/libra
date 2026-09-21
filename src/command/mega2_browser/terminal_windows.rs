//! Windows terminal lifecycle via direct `windows-sys` console APIs.
//! Uses the `Win32_System_Console` feature (plan-20260912 MB-02 T-1).

use std::io::{self, Write};

use windows_sys::Win32::{
    Foundation::{HANDLE, INVALID_HANDLE_VALUE},
    System::Console::{self, GetConsoleMode, GetStdHandle, STD_INPUT_HANDLE, SetConsoleMode},
};

use crate::utils::error::{CliError, CliResult, StableErrorCode};

const ENTER_ALT_SCREEN: &[u8] = b"\x1b[?1049h\x1b[?25l";
const LEAVE_ALT_SCREEN: &[u8] = b"\x1b[?1049l\x1b[?25h";

/// Console-mode + alternate-screen guard for Windows.
pub struct WindowsTerminalGuard {
    handle: HANDLE,
    saved_mode: u32,
    restored: bool,
}

impl WindowsTerminalGuard {
    pub fn enter() -> CliResult<Self> {
        // SAFETY: passing -10 requests STD_INPUT_HANDLE; null means failure.
        let handle = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            return Err(
                CliError::fatal("mega2 browser: cannot access the console input handle")
                    .with_stable_code(StableErrorCode::IoReadFailed),
            );
        }

        let mut saved_mode: u32 = 0;
        // SAFETY: handle is a valid console input handle and saved_mode is writable.
        if unsafe { GetConsoleMode(handle, &mut saved_mode) } == 0 {
            return Err(CliError::fatal(format!(
                "mega2 browser: cannot read console mode: {}",
                io::Error::last_os_error()
            ))
            .with_stable_code(StableErrorCode::IoReadFailed));
        }

        // Raw-ish mode: disable line input and echo; keep processed input off too
        // so key events arrive as they are typed.
        let raw_mode = saved_mode
            & !(Console::ENABLE_LINE_INPUT
                | Console::ENABLE_ECHO_INPUT
                | Console::ENABLE_PROCESSED_INPUT);
        // SAFETY: handle valid, raw_mode derived from the saved bitmask.
        if unsafe { SetConsoleMode(handle, raw_mode) } == 0 {
            return Err(CliError::fatal(format!(
                "mega2 browser: cannot enter raw console mode: {}",
                io::Error::last_os_error()
            ))
            .with_stable_code(StableErrorCode::IoWriteFailed));
        }

        let guard = Self {
            handle,
            saved_mode,
            restored: false,
        };
        guard.write_all(ENTER_ALT_SCREEN).map_err(|e| {
            CliError::fatal(format!("mega2 browser: cannot enter alternate screen: {e}"))
                .with_stable_code(StableErrorCode::IoWriteFailed)
        })?;
        Ok(guard)
    }

    fn write_all(&self, bytes: &[u8]) -> io::Result<()> {
        let mut stdout = io::stdout().lock();
        stdout.write_all(bytes)?;
        stdout.flush()
    }

    /// Best-effort, idempotent restore.
    pub fn restore(&mut self) -> io::Result<()> {
        let mut first_error = None;
        if !self.restored {
            self.restored = true;
            if self.write_all(LEAVE_ALT_SCREEN).is_err() {
                first_error = Some(io::Error::other("failed to leave the alternate screen"));
            }
            // SAFETY: handle is the console input handle we entered with; saved_mode
            // is the mode captured before entering.
            if unsafe { SetConsoleMode(self.handle, self.saved_mode) } == 0 && first_error.is_none()
            {
                first_error = Some(io::Error::last_os_error());
            }
        }
        match first_error {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

impl Drop for WindowsTerminalGuard {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}
