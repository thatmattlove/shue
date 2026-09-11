use std::ffi::{OsStr, OsString};
use std::io::{Read, Write};

use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use thiserror::Error;

use crate::TerminalSize;

/// A pseudo-terminal lifecycle failure.
#[derive(Debug, Error)]
pub enum PtyError {
    #[error("failed to open a pseudo-terminal: {message}")]
    Open { message: String },

    #[error("failed to spawn {program:?} in the pseudo-terminal: {message}")]
    Spawn { program: OsString, message: String },

    #[error("failed to clone the pseudo-terminal reader: {message}")]
    CloneReader { message: String },

    #[error("failed to take the pseudo-terminal writer: {message}")]
    TakeWriter { message: String },

    #[error("failed to resize the pseudo-terminal: {message}")]
    Resize { message: String },

    #[error("failed while waiting for the pseudo-terminal child: {message}")]
    Wait { message: String },

    #[error("failed to terminate the pseudo-terminal child: {message}")]
    Terminate { message: String },
}

/// Result type for PTY operations.
pub type Result<T> = std::result::Result<T, PtyError>;

/// A child process attached to a native pseudo-terminal.
///
/// The command is spawned directly; no command shell is involved. The PTY's
/// slave side is dropped after spawning so readers receive EOF once the child
/// and any descendants close their terminal handles.
/// Independently owned readers and writers should be drained or dropped before
/// the session is dropped, so that cleanup can close the terminal completely.
pub struct PtySession {
    master: Option<Box<dyn MasterPty + Send>>,
    child: Box<dyn Child + Send + Sync>,
}

impl PtySession {
    /// Spawn `program` with the supplied OS-native argument values.
    pub fn spawn(program: &OsStr, args: &[OsString], size: TerminalSize) -> Result<Self> {
        let pair = native_pty_system()
            .openpty(size.into())
            .map_err(|error| PtyError::Open {
                message: error.to_string(),
            })?;

        let mut command = CommandBuilder::new(program);
        command.args(args);
        let child = pair
            .slave
            .spawn_command(command)
            .map_err(|error| PtyError::Spawn {
                program: program.to_os_string(),
                message: error.to_string(),
            })?;
        drop(pair.slave);

        Ok(Self {
            master: Some(pair.master),
            child,
        })
    }

    /// Return an independently owned reader for the PTY output stream.
    pub fn try_clone_reader(&self) -> Result<Box<dyn Read + Send>> {
        self.master()
            .try_clone_reader()
            .map_err(|error| PtyError::CloneReader {
                message: error.to_string(),
            })
    }

    /// Take the PTY input stream.
    ///
    /// Portable PTYs expose a single writer; a second call returns an error.
    pub fn take_writer(&self) -> Result<Box<dyn Write + Send>> {
        self.master()
            .take_writer()
            .map_err(|error| PtyError::TakeWriter {
                message: error.to_string(),
            })
    }

    /// Update the PTY dimensions and notify the child through native PTY
    /// semantics (for example, `SIGWINCH` on Unix).
    pub fn resize(&self, size: TerminalSize) -> Result<()> {
        self.master()
            .resize(size.into())
            .map_err(|error| PtyError::Resize {
                message: error.to_string(),
            })
    }

    /// Request termination of the child process using the portable PTY's
    /// native termination mechanism (SIGHUP on Unix).
    ///
    /// This is intended for local shutdown signals and fatal I/O paths where
    /// the caller must stop the wrapped program before returning and restoring
    /// terminal state.
    pub fn terminate(&mut self) -> Result<()> {
        self.child.kill().map_err(|error| PtyError::Terminate {
            message: error.to_string(),
        })
    }

    /// Wait for the child and return its shell-compatible exit code.
    ///
    /// Drain the PTY output before or concurrently with this call. A child can
    /// block while writing output, and on macOS its exit can also wait for the
    /// controlling terminal's remaining output to be consumed.
    ///
    /// On Unix, signal termination is returned as `128 + signal`, matching
    /// conventional shell and OpenSSH wrapper behavior.
    pub fn wait(&mut self) -> Result<u32> {
        #[cfg(unix)]
        if let Some(child) =
            (&mut *self.child as &mut dyn Child).downcast_mut::<std::process::Child>()
        {
            return child
                .wait()
                .map(unix_exit_code)
                .map_err(|error| PtyError::Wait {
                    message: error.to_string(),
                });
        }

        self.child
            .wait()
            .map(|status| status.exit_code())
            .map_err(|error| PtyError::Wait {
                message: error.to_string(),
            })
    }

    /// Poll the child without blocking, returning its shell-compatible exit
    /// code once it has exited or `None` while it is still running.
    ///
    /// As with [`Self::wait`], the PTY output must be drained so that the child
    /// can finish writing and exit. On Unix, signal termination is returned as
    /// `128 + signal`.
    pub fn try_wait(&mut self) -> Result<Option<u32>> {
        #[cfg(unix)]
        if let Some(child) =
            (&mut *self.child as &mut dyn Child).downcast_mut::<std::process::Child>()
        {
            return child
                .try_wait()
                .map(|status| status.map(unix_exit_code))
                .map_err(|error| PtyError::Wait {
                    message: error.to_string(),
                });
        }

        self.child
            .try_wait()
            .map(|status| status.map(|status| status.exit_code()))
            .map_err(|error| PtyError::Wait {
                message: error.to_string(),
            })
    }

    fn master(&self) -> &(dyn MasterPty + Send) {
        self.master
            .as_deref()
            .expect("PTY master is present until the session is dropped")
    }
}

#[cfg(unix)]
fn unix_exit_code(status: std::process::ExitStatus) -> u32 {
    use std::os::unix::process::ExitStatusExt;

    status
        .code()
        .map(|code| code as u32)
        .or_else(|| status.signal().map(|signal| 128 + signal as u32))
        .unwrap_or(1)
}

impl Drop for PtySession {
    fn drop(&mut self) {
        // A macOS session leader can wait for pending terminal output during
        // exit. Close our master before reaping it so an abandoned session
        // cannot deadlock cleanup when its output reader has already gone.
        drop(self.master.take());
        if !matches!(self.child.try_wait(), Ok(Some(_))) && self.child.kill().is_ok() {
            let _ = self.child.wait();
        }
    }
}

impl From<TerminalSize> for PtySize {
    fn from(size: TerminalSize) -> Self {
        Self {
            rows: size.rows,
            cols: size.cols,
            pixel_width: size.pixel_width,
            pixel_height: size.pixel_height,
        }
    }
}
