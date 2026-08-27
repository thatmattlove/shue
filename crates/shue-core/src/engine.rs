use std::ops::Range;
use std::sync::atomic::Ordering;

use thiserror::Error;

use crate::ansi::{SgrState, TokenKind, Tokenized};
use crate::color::Style;
use crate::config::Config;

/// A runtime matching or rendering failure.
#[derive(Debug, Error)]
pub enum HighlightError {
    #[error("PCRE2 matching failed for rule {rule}: {source}")]
    Regex {
        rule: usize,
        #[source]
        source: pcre2::Error,
    },
}

#[derive(Clone, Copy, Debug)]
struct Paint {
    start: usize,
    end: usize,
    style: Style,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Event {
    position: usize,
    paint: usize,
    starts: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct StyledSpan {
    start: usize,
    end: usize,
    style: Style,
}

pub(crate) fn highlight_with_state(
    config: &Config,
    input: &[u8],
    raw_limit: usize,
    output: &mut Vec<u8>,
    state: &mut SgrState,
) -> Result<(), HighlightError> {
    debug_assert!(raw_limit <= input.len());
    let tokenized = Tokenized::new(input);
    let segments = matching_segments(tokenized.subject.as_ref(), &tokenized.barriers);
    let paints = collect_paints(config, tokenized.subject.as_ref(), &segments);
    output.reserve(raw_limit.saturating_add(paints.len().saturating_mul(24)));
    let spans = sweep_spans(&paints);
    render(input, raw_limit, &tokenized, &spans, output, state);
    Ok(())
}

fn collect_paints(config: &Config, subject: &[u8], segments: &[Range<usize>]) -> Vec<Paint> {
    let mut paints = Vec::new();
    let mut masked: Option<Vec<u8>> = None;

    for (rule_index, rule) in config.rules.iter().enumerate() {
        if rule.disabled.load(Ordering::Acquire) {
            continue;
        }
        let haystack = masked.as_deref().unwrap_or(subject);
        let mut rule_paints = Vec::new();
        let mut locations = rule.regex.capture_locations();
        let mut failed = false;
        for segment in segments {
            let haystack_segment = &haystack[segment.clone()];
            let mut next_offset = 0;
            let mut previous_end = None;
            while next_offset <= haystack_segment.len() {
                let matched_rule =
                    match rule
                        .regex
                        .captures_read_at(&mut locations, haystack_segment, next_offset)
                    {
                        Ok(Some(matched_rule)) => matched_rule,
                        Ok(None) => break,
                        Err(source) => {
                            config.quarantine_rule(rule_index, &source);
                            rule_paints.clear();
                            failed = true;
                            break;
                        }
                    };
                let empty = matched_rule.start() == matched_rule.end();
                next_offset = if empty {
                    matched_rule.end() + 1
                } else {
                    matched_rule.end()
                };
                // Match pcre2::bytes::CaptureMatches' empty-match semantics
                // while reusing one capture vector for the entire rule. This
                // removes a heap allocation per match on dense terminal output.
                let skip_adjacent_empty = empty && previous_end == Some(matched_rule.end());
                previous_end = Some(matched_rule.end());
                if skip_adjacent_empty {
                    continue;
                }

                for group in &rule.groups {
                    let Some((local_start, local_end)) = locations.get(group.group) else {
                        continue;
                    };
                    let start = segment.start + local_start;
                    let end = segment.start + local_end;
                    // Newlines may still occur here when an earlier exclusive
                    // match was masked. Keep ChromaTerm's rule-level behavior
                    // of refusing to color across masked regions.
                    if start == end
                        || haystack[start..end]
                            .iter()
                            .any(|byte| matches!(byte, b'\r' | b'\n' | 0x0b | 0x0c))
                    {
                        continue;
                    }
                    rule_paints.push(Paint {
                        start,
                        end,
                        style: group.style,
                    });
                }
            }
            if failed {
                break;
            }
        }

        if failed {
            continue;
        }

        if rule.exclusive && !rule_paints.is_empty() {
            let masked = masked.get_or_insert_with(|| subject.to_vec());
            for paint in &rule_paints {
                masked[paint.start..paint.end].fill(b'\n');
            }
        }
        paints.extend(rule_paints);
    }
    paints
}

/// Match the same pieces ChromaTerm's stream splitter treats as data. Vertical
/// separators and non-SGR controls are deliberately outside every segment;
/// regex anchors therefore restart at screen-record boundaries and a broad
/// alternative cannot consume a separator and hide a later valid match.
fn matching_segments(subject: &[u8], barriers: &[usize]) -> Vec<Range<usize>> {
    let mut segments = Vec::new();
    let mut start = 0;
    let mut cursor = 0;
    let mut barrier = 0;

    while cursor < subject.len() {
        if barriers.get(barrier) == Some(&cursor) {
            if start < cursor {
                segments.push(start..cursor);
            }
            start = cursor;
            while barriers.get(barrier) == Some(&cursor) {
                barrier += 1;
            }
        }

        let separator_len = match subject[cursor] {
            b'\r' if subject.get(cursor + 1) == Some(&b'\n') => 2,
            b'\r' | b'\n' | 0x0b | 0x0c => 1,
            _ => 0,
        };
        if separator_len != 0 {
            if start < cursor {
                segments.push(start..cursor);
            }
            cursor += separator_len;
            start = cursor;
        } else {
            cursor += 1;
        }
    }

    if start < subject.len() {
        segments.push(start..subject.len());
    }
    segments
}

fn sweep_spans(paints: &[Paint]) -> Vec<StyledSpan> {
    if paints.is_empty() {
        return Vec::new();
    }
    let mut events = Vec::with_capacity(paints.len() * 2);
    for (paint, item) in paints.iter().enumerate() {
        events.push(Event {
            position: item.start,
            paint,
            starts: true,
        });
        events.push(Event {
            position: item.end,
            paint,
            starts: false,
        });
    }
    events.sort_unstable_by_key(|event| (event.position, event.starts, event.paint));

    let mut spans: Vec<StyledSpan> = Vec::new();
    // The active overlap count is normally bounded by rule/group count, while
    // total matches can be enormous. A sorted small vector avoids one tree-node
    // allocation for every start event and still preserves paint priority.
    let mut active: Vec<usize> = Vec::new();
    let mut previous = events[0].position;
    let mut cursor = 0;
    while cursor < events.len() {
        let position = events[cursor].position;
        if previous < position && !active.is_empty() {
            let mut style = Style::default();
            for paint in active.iter().copied() {
                style.overlay(paints[paint].style);
            }
            if !style.is_empty() {
                if let Some(last) = spans.last_mut() {
                    if last.end == previous && last.style == style {
                        last.end = position;
                    } else {
                        spans.push(StyledSpan {
                            start: previous,
                            end: position,
                            style,
                        });
                    }
                } else {
                    spans.push(StyledSpan {
                        start: previous,
                        end: position,
                        style,
                    });
                }
            }
        }

        while cursor < events.len() && events[cursor].position == position {
            let event = events[cursor];
            if event.starts {
                let index = active
                    .binary_search(&event.paint)
                    .unwrap_or_else(|index| index);
                active.insert(index, event.paint);
            } else if let Ok(index) = active.binary_search(&event.paint) {
                active.remove(index);
            }
            cursor += 1;
        }
        previous = position;
    }
    spans
}

fn render(
    input: &[u8],
    raw_limit: usize,
    tokenized: &Tokenized<'_>,
    spans: &[StyledSpan],
    output: &mut Vec<u8>,
    state: &mut SgrState,
) {
    let mut active_style = None;
    let mut span_cursor = 0;

    for token in &tokenized.tokens {
        if token.raw_start >= raw_limit {
            break;
        }
        let raw_end = token.raw_end.min(raw_limit);
        match token.kind {
            TokenKind::Text => {
                let visible_end = token.visible_start + (raw_end - token.raw_start);
                let mut visible = token.visible_start;
                let mut raw = token.raw_start;
                while visible < visible_end {
                    while span_cursor < spans.len() && spans[span_cursor].end <= visible {
                        span_cursor += 1;
                    }
                    let desired = spans.get(span_cursor).and_then(|span| {
                        (span.start <= visible && visible < span.end).then_some(span.style)
                    });
                    transition(active_style, desired, *state, output);
                    active_style = desired;

                    let next = match spans.get(span_cursor) {
                        Some(span) if visible < span.start => span.start.min(visible_end),
                        Some(span) if visible < span.end => span.end.min(visible_end),
                        _ => visible_end,
                    };
                    let count = next - visible;
                    output.extend_from_slice(&input[raw..raw + count]);
                    raw += count;
                    visible = next;
                }
            }
            TokenKind::Control { sgr, .. } => {
                // Close before and reopen after the indivisible control. This
                // prevents an inserted SGR from ever becoming OSC/DCS payload
                // and lets original SGR changes update the underlying state.
                if let Some(style) = active_style.take() {
                    state.emit_restore(style, output);
                }
                output.extend_from_slice(&input[token.raw_start..raw_end]);
                if sgr && raw_end == token.raw_end {
                    state.update(&input[token.raw_start..token.raw_end]);
                }
                if raw_end == token.raw_end {
                    let desired = style_at(spans, token.visible_start, &mut span_cursor);
                    if let Some(style) = desired {
                        style.emit_open(output);
                        active_style = Some(style);
                    }
                }
            }
        }
    }
    if let Some(style) = active_style {
        state.emit_restore(style, output);
    }
}

fn style_at(spans: &[StyledSpan], position: usize, cursor: &mut usize) -> Option<Style> {
    while *cursor < spans.len() && spans[*cursor].end <= position {
        *cursor += 1;
    }
    spans
        .get(*cursor)
        .and_then(|span| (span.start <= position && position < span.end).then_some(span.style))
}

fn transition(old: Option<Style>, new: Option<Style>, state: SgrState, output: &mut Vec<u8>) {
    if old == new {
        return;
    }
    if let Some(style) = old {
        state.emit_restore(style, output);
    }
    if let Some(style) = new {
        style.emit_open(output);
    }
}
