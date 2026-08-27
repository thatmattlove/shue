use std::io;
use std::sync::Mutex;

use crossterm::terminal;

/// Character-cell and pixel dimensions for a terminal or pseudo-terminal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TerminalSize {
    pub rows: u16,
    pub cols: u16,
    pub pixel_width: u16,
    pub pixel_height: u16,
}

impl TerminalSize {
    /// Query the terminal associated with the current process.
    pub fn current() -> io::Result<Self> {
        terminal::window_size().map(|size| Self {
            rows: size.rows,
            cols: size.columns,
            pixel_width: size.width,
            pixel_height: size.height,
        })
    }
}

impl Default for TerminalSize {
    fn default() -> Self {
        Self {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        }
    }
}

#[derive(Debug, Default)]
struct RawModeState {
    leases: usize,
    restore_when_unused: bool,
}

static RAW_MODE_STATE: Mutex<RawModeState> = Mutex::new(RawModeState {
    leases: 0,
    restore_when_unused: false,
});

/// An RAII lease on the process terminal's raw-input mode.
///
/// Guards may be nested. Raw mode is enabled for the first live guard and the
/// previous terminal mode is restored when the final guard is explicitly
/// restored or dropped, including while unwinding an ordinary error path.
#[derive(Debug)]
pub struct RawModeGuard {
    active: bool,
}

impl RawModeGuard {
    /// Enable terminal raw mode and return a guard which restores it.
    pub fn enable() -> io::Result<Self> {
        let mut state = RAW_MODE_STATE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        if state.leases == usize::MAX {
            return Err(io::Error::other("raw-mode guard count overflow"));
        }
        if state.leases == 0 {
            let was_raw = terminal::is_raw_mode_enabled()?;
            if !was_raw {
                terminal::enable_raw_mode()?;
            }
            state.restore_when_unused = !was_raw;
        }
        state.leases += 1;

        Ok(Self { active: true })
    }

    /// Restore the prior terminal mode if this guard is still active.
    ///
    /// The method is idempotent. If the final restore attempt fails, the guard
    /// remains active so its subsequent `Drop` gets one more restoration
    /// attempt.
    pub fn restore(&mut self) -> io::Result<()> {
        if !self.active {
            return Ok(());
        }

        let mut state = RAW_MODE_STATE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match state.leases {
            0 => {
                self.active = false;
            }
            1 => {
                if state.restore_when_unused {
                    terminal::disable_raw_mode()?;
                }
                state.leases = 0;
                state.restore_when_unused = false;
                self.active = false;
            }
            _ => {
                state.leases -= 1;
                self.active = false;
            }
        }
        Ok(())
    }

    /// Whether this guard still owns a raw-mode lease.
    pub fn is_active(&self) -> bool {
        self.active
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}
