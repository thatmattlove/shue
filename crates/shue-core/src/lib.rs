//! High-performance, binary-safe terminal highlighting for `shue`.
//!
//! Patterns are compiled as PCRE2 byte regular expressions, with JIT enabled
//! whenever the linked PCRE2 supports it. Matching is performed on a view that
//! omits terminal escape sequences; rendering keeps the original byte stream
//! intact and adds SGR styling only at text boundaries.

mod ansi;
mod color;
mod config;
mod engine;
mod stream;

pub use config::{
    ColorDepth, Config, ConfigError, ConfigWarning, RuntimeWarning, detect_color_depth,
};
pub use engine::HighlightError;
pub use stream::{MAX_PENDING_BYTES, STREAM_OVERLAP_BYTES, StreamHighlighter};
