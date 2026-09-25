//! plan-20260912 MB-02: cross-platform terminal lifecycle for `libra mega2 browser`.
//!
//! Only direct `libc` (Unix termios) and `windows-sys` (console mode) are used —
//! no third-party TUI crates (enforced by `compat_agent_architecture_guard`).

use crate::utils::error::{CliError, CliResult, StableErrorCode};

/// RAII terminal guard: entering raw mode + alternate screen, restoring on drop.
pub struct TerminalGuard {
    #[cfg(unix)]
    inner: super::terminal_unix::UnixTerminalGuard,
    #[cfg(windows)]
    inner: super::terminal_windows::WindowsTerminalGuard,
}

impl TerminalGuard {
    /// Enters raw mode and the alternate screen. On unsupported platforms this
    /// fails **before** any terminal alteration.
    pub fn enter() -> CliResult<Self> {
        select_platform_impl(cfg!(unix), cfg!(windows))?;
        #[cfg(unix)]
        {
            Ok(Self {
                inner: super::terminal_unix::UnixTerminalGuard::enter()?,
            })
        }
        #[cfg(windows)]
        {
            Ok(Self {
                inner: super::terminal_windows::WindowsTerminalGuard::enter()?,
            })
        }
        #[cfg(not(any(unix, windows)))]
        {
            // Unreachable: select_platform_impl already returned an error.
            Err(unsupported_platform_error())
        }
    }
}

/// Pure platform selection, testable on every platform (fail-closed seam).
pub fn select_platform_impl(is_unix: bool, is_windows: bool) -> CliResult<()> {
    if !is_unix && !is_windows {
        return Err(unsupported_platform_error());
    }
    Ok(())
}

impl TerminalGuard {
    /// Best-effort, idempotent restore of termios/console mode and screen state.
    pub fn restore(&mut self) -> std::io::Result<()> {
        #[cfg(unix)]
        {
            self.inner.restore()
        }
        #[cfg(windows)]
        {
            self.inner.restore()
        }
        #[cfg(not(any(unix, windows)))]
        {
            Ok(())
        }
    }
}

fn unsupported_platform_error() -> CliError {
    CliError::fatal(
        "mega2 browser: this platform has no terminal adapter (refusing to alter the terminal)",
    )
    .with_stable_code(StableErrorCode::Unsupported)
}
