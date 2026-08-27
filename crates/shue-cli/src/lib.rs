//! Command-line parsing and configuration discovery for `shue`.
//!
//! The parser is deliberately byte/OS-string preserving. Once it encounters the
//! first SSH argument, it stops interpreting options so remote commands cannot
//! accidentally be consumed as `shue` options.

pub mod cli;
pub mod config;
