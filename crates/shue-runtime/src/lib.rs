//! Process and terminal primitives used by the `shue` command-line program.
//!
//! The crate intentionally does not invoke a shell. Programs and arguments stay
//! as [`std::ffi::OsStr`] / [`std::ffi::OsString`] values until they are passed
//! to the operating system.

#![forbid(unsafe_code)]

mod pty;
mod resolve;
mod terminal;

pub use pty::{PtyError, PtySession, Result};
pub use resolve::{
    ResolveError, SshPathOptions, SshPathSource, resolve_ssh_path, resolve_ssh_path_from_env,
};
pub use terminal::{RawModeGuard, TerminalSize};
