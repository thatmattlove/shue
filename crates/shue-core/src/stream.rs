use crate::ansi::{SgrState, TokenKind, Tokenized, last_record_boundary, safe_raw_boundary};
use crate::config::{Config, RuntimeWarning};
use crate::engine::{HighlightError, highlight_with_state};

/// Hard upper bound for ordinarily buffered, unterminated stream data.
pub const MAX_PENDING_BYTES: usize = 64 * 1024;
/// Context retained when a pathological line exceeds [`MAX_PENDING_BYTES`].
pub const STREAM_OVERLAP_BYTES: usize = 8 * 1024;
const MAX_CAPTURED_CONTROL_BYTES: usize = 4 * 1024;

/// Incremental highlighter that joins normal reads, flushes complete records,
/// and provides an explicit reusable idle flush for interactive prompts.
pub struct StreamHighlighter {
    config: Config,
    pending: Vec<u8>,
    state: SgrState,
    opaque: Option<OpaqueState>,
}

impl StreamHighlighter {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            pending: Vec::with_capacity(MAX_PENDING_BYTES),
            state: SgrState::default(),
            opaque: None,
        }
    }

    /// Append a read. Complete terminal records are emitted promptly; a
    /// trailing partial record remains buffered so matches may cross reads.
    pub fn push(&mut self, mut input: &[u8], output: &mut Vec<u8>) -> Result<(), HighlightError> {
        while !input.is_empty() {
            if self.opaque.is_some() {
                let (consumed, complete) = self.consume_opaque(input, output);
                input = &input[consumed..];
                if !complete {
                    return Ok(());
                }
                continue;
            }

            if self.pending.len() == MAX_PENDING_BYTES {
                self.force_bounded_drain(output)?;
                continue;
            }
            let available = MAX_PENDING_BYTES - self.pending.len();
            let consumed = available.min(input.len());
            self.pending.extend_from_slice(&input[..consumed]);
            input = &input[consumed..];

            self.drain_complete_records(output)?;
            if self.pending.len() == MAX_PENDING_BYTES {
                self.force_bounded_drain(output)?;
            }
        }
        Ok(())
    }

    /// Emit all pending data now. The highlighter remains reusable afterwards.
    ///
    /// Calling this for an idle prompt intentionally commits a stream boundary:
    /// later input cannot retroactively form a regex match with emitted bytes.
    pub fn flush(&mut self, output: &mut Vec<u8>) -> Result<(), HighlightError> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let trailing = trailing_incomplete_control(&self.pending)
            .and_then(|start| OpaqueState::from_incomplete(&self.pending[start..]));
        highlight_with_state(
            &self.config,
            &self.pending,
            self.pending.len(),
            output,
            &mut self.state,
        )?;
        self.pending.clear();
        self.opaque = trailing;
        Ok(())
    }

    /// Number of bytes retained for possible cross-read matches.
    pub fn buffered_len(&self) -> usize {
        self.pending.len()
    }

    /// Drain once-only warnings for regexes quarantined while streaming.
    pub fn take_runtime_warnings(&self) -> Vec<RuntimeWarning> {
        self.config.take_runtime_warnings()
    }

    /// Count rules quarantined by runtime PCRE2 failures.
    pub fn disabled_rule_count(&self) -> usize {
        self.config.disabled_rule_count()
    }

    fn drain_complete_records(&mut self, output: &mut Vec<u8>) -> Result<(), HighlightError> {
        let Some(boundary) = last_record_boundary(&self.pending) else {
            return Ok(());
        };
        self.drain_prefix(boundary, boundary, output)
    }

    fn force_bounded_drain(&mut self, output: &mut Vec<u8>) -> Result<(), HighlightError> {
        debug_assert_eq!(self.pending.len(), MAX_PENDING_BYTES);
        let desired = self.pending.len() - STREAM_OVERLAP_BYTES;
        let boundary = safe_raw_boundary(&self.pending, desired);
        if boundary != 0 {
            return self.drain_prefix(boundary, self.pending.len(), output);
        }

        // A single oversized control sequence crossed the hard boundary. Emit
        // it byte-for-byte without styling and remain in opaque passthrough
        // until its terminator arrives; this keeps memory bounded and guarantees
        // no SGR bytes can be inserted into OSC/DCS/CSI payload.
        let opaque = trailing_incomplete_control(&self.pending)
            .and_then(|start| OpaqueState::from_incomplete(&self.pending[start..]));
        highlight_with_state(
            &self.config,
            &self.pending,
            self.pending.len(),
            output,
            &mut self.state,
        )?;
        self.pending.clear();
        self.opaque = opaque;
        Ok(())
    }

    fn drain_prefix(
        &mut self,
        boundary: usize,
        analysis_end: usize,
        output: &mut Vec<u8>,
    ) -> Result<(), HighlightError> {
        highlight_with_state(
            &self.config,
            &self.pending[..analysis_end],
            boundary,
            output,
            &mut self.state,
        )?;
        self.pending.drain(..boundary);
        Ok(())
    }

    fn consume_opaque(&mut self, input: &[u8], output: &mut Vec<u8>) -> (usize, bool) {
        let state = self.opaque.as_mut().expect("checked by caller");
        let (consumed, complete) = state.consume(input);
        output.extend_from_slice(&input[..consumed]);
        if complete {
            let state = self.opaque.take().expect("present until completion");
            if state.final_byte == Some(b'm') && !state.capture_overflow {
                self.state.update(&state.captured);
            }
        }
        (consumed, complete)
    }
}

fn trailing_incomplete_control(input: &[u8]) -> Option<usize> {
    let tokenized = Tokenized::new(input);
    tokenized.tokens.last().and_then(|token| match token.kind {
        TokenKind::Control {
            complete: false, ..
        } if token.raw_end == input.len() => Some(token.raw_start),
        _ => None,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OpaqueKind {
    EscStart,
    Escape,
    Csi,
    Osc,
    String,
}

#[derive(Debug)]
struct OpaqueState {
    kind: OpaqueKind,
    previous_escape: bool,
    captured: Vec<u8>,
    capture_overflow: bool,
    final_byte: Option<u8>,
}

impl OpaqueState {
    fn from_incomplete(sequence: &[u8]) -> Option<Self> {
        let kind = match sequence {
            [0x1b] => OpaqueKind::EscStart,
            [0x1b, b'[', ..] | [0x9b, ..] => OpaqueKind::Csi,
            [0x1b, b']', ..] | [0x9d, ..] => OpaqueKind::Osc,
            [0x1b, b'P' | b'X' | b'^' | b'_', ..] | [0x90 | 0x98 | 0x9e | 0x9f, ..] => {
                OpaqueKind::String
            }
            [0x1b, ..] => OpaqueKind::Escape,
            _ => return None,
        };
        let capture_overflow = sequence.len() > MAX_CAPTURED_CONTROL_BYTES;
        Some(Self {
            kind,
            previous_escape: sequence.last() == Some(&0x1b),
            captured: if capture_overflow {
                Vec::new()
            } else {
                sequence.to_vec()
            },
            capture_overflow,
            final_byte: None,
        })
    }

    fn consume(&mut self, input: &[u8]) -> (usize, bool) {
        for (index, byte) in input.iter().copied().enumerate() {
            if !self.capture_overflow {
                if self.captured.len() < MAX_CAPTURED_CONTROL_BYTES {
                    self.captured.push(byte);
                } else {
                    self.captured.clear();
                    self.capture_overflow = true;
                }
            }

            let complete = match self.kind {
                OpaqueKind::EscStart => match byte {
                    b'[' => {
                        self.kind = OpaqueKind::Csi;
                        false
                    }
                    b']' => {
                        self.kind = OpaqueKind::Osc;
                        false
                    }
                    b'P' | b'X' | b'^' | b'_' => {
                        self.kind = OpaqueKind::String;
                        false
                    }
                    0x20..=0x2f => {
                        self.kind = OpaqueKind::Escape;
                        false
                    }
                    _ => true,
                },
                OpaqueKind::Escape => (0x30..=0x7e).contains(&byte),
                OpaqueKind::Csi => (0x40..=0x7e).contains(&byte),
                OpaqueKind::Osc => {
                    byte == 0x07 || byte == 0x9c || (self.previous_escape && byte == b'\\')
                }
                OpaqueKind::String => byte == 0x9c || (self.previous_escape && byte == b'\\'),
            };
            self.previous_escape = byte == 0x1b;
            if complete {
                self.final_byte = Some(byte);
                return (index + 1, true);
            }
        }
        (input.len(), false)
    }
}
