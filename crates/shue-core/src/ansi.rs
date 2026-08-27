use std::borrow::Cow;

use crate::color::{
    BLINK, BOLD, DIM, HIDDEN, INVERT, ITALIC, RenderedColor, STRIKE, SgrParameters, Style,
    UNDERLINE, emit_sgr,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TokenKind {
    Text,
    Control { sgr: bool, complete: bool },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Token {
    pub(crate) raw_start: usize,
    pub(crate) raw_end: usize,
    pub(crate) visible_start: usize,
    pub(crate) visible_end: usize,
    pub(crate) kind: TokenKind,
}

#[derive(Debug)]
pub(crate) struct Tokenized<'a> {
    pub(crate) subject: Cow<'a, [u8]>,
    pub(crate) tokens: Vec<Token>,
    /// Visible offsets at which a non-SGR terminal control splits matching.
    pub(crate) barriers: Vec<usize>,
}

impl<'a> Tokenized<'a> {
    pub(crate) fn new(input: &'a [u8]) -> Self {
        if !input.iter().copied().any(is_control_starter) {
            return Self {
                subject: Cow::Borrowed(input),
                tokens: if input.is_empty() {
                    Vec::new()
                } else {
                    vec![Token {
                        raw_start: 0,
                        raw_end: input.len(),
                        visible_start: 0,
                        visible_end: input.len(),
                        kind: TokenKind::Text,
                    }]
                },
                barriers: Vec::new(),
            };
        }

        let mut subject = Vec::with_capacity(input.len());
        let mut tokens = Vec::new();
        let mut barriers = Vec::new();
        let mut raw = 0;
        let mut text_start = 0;
        while raw < input.len() {
            if !is_control_starter(input[raw]) {
                raw += 1;
                continue;
            }

            if text_start < raw {
                push_text(input, text_start, raw, &mut subject, &mut tokens);
            }
            let visible = subject.len();
            let (end, sgr, complete) = parse_control(input, raw);
            tokens.push(Token {
                raw_start: raw,
                raw_end: end,
                visible_start: visible,
                visible_end: visible,
                kind: TokenKind::Control { sgr, complete },
            });
            if !sgr {
                barriers.push(visible);
            }
            raw = end;
            text_start = raw;
        }
        if text_start < input.len() {
            push_text(input, text_start, input.len(), &mut subject, &mut tokens);
        }
        Self {
            subject: Cow::Owned(subject),
            tokens,
            barriers,
        }
    }
}

fn push_text(
    input: &[u8],
    raw_start: usize,
    raw_end: usize,
    subject: &mut Vec<u8>,
    tokens: &mut Vec<Token>,
) {
    let visible_start = subject.len();
    subject.extend_from_slice(&input[raw_start..raw_end]);
    tokens.push(Token {
        raw_start,
        raw_end,
        visible_start,
        visible_end: subject.len(),
        kind: TokenKind::Text,
    });
}

fn is_control_starter(byte: u8) -> bool {
    matches!(byte, 0x1b | 0x90 | 0x98 | 0x9b | 0x9d | 0x9e | 0x9f)
}

fn parse_control(input: &[u8], start: usize) -> (usize, bool, bool) {
    match input[start] {
        0x1b => parse_escape(input, start),
        0x9b => parse_csi(input, start, start + 1),
        0x9d => parse_string_control(input, start, start + 1, true),
        0x90 | 0x98 | 0x9e | 0x9f => parse_string_control(input, start, start + 1, false),
        _ => unreachable!("caller checks control starter"),
    }
}

fn parse_escape(input: &[u8], start: usize) -> (usize, bool, bool) {
    let Some(&second) = input.get(start + 1) else {
        return (input.len(), false, false);
    };
    match second {
        b'[' => parse_csi(input, start, start + 2),
        b']' => parse_string_control(input, start, start + 2, true),
        b'P' | b'X' | b'^' | b'_' => parse_string_control(input, start, start + 2, false),
        _ => {
            let mut cursor = start + 1;
            while input
                .get(cursor)
                .is_some_and(|byte| (0x20..=0x2f).contains(byte))
            {
                cursor += 1;
            }
            if input
                .get(cursor)
                .is_some_and(|byte| (0x30..=0x7e).contains(byte))
            {
                (cursor + 1, false, true)
            } else if cursor == start + 1 {
                // An ESC followed by an unsupported Fe/C0 byte is still kept
                // indivisible so styling is never inserted between the pair.
                ((start + 2).min(input.len()), false, true)
            } else {
                (input.len(), false, false)
            }
        }
    }
}

fn parse_csi(input: &[u8], _start: usize, mut cursor: usize) -> (usize, bool, bool) {
    while let Some(&byte) = input.get(cursor) {
        if (0x40..=0x7e).contains(&byte) {
            return (cursor + 1, byte == b'm', true);
        }
        cursor += 1;
    }
    (input.len(), false, false)
}

fn parse_string_control(
    input: &[u8],
    _start: usize,
    mut cursor: usize,
    bell_terminated: bool,
) -> (usize, bool, bool) {
    while cursor < input.len() {
        if bell_terminated && input[cursor] == 0x07 {
            return (cursor + 1, false, true);
        }
        if input[cursor] == 0x9c {
            return (cursor + 1, false, true);
        }
        if input[cursor] == 0x1b && input.get(cursor + 1) == Some(&b'\\') {
            return (cursor + 2, false, true);
        }
        cursor += 1;
    }
    (input.len(), false, false)
}

/// Return the last complete terminal-record boundary. Vertical separators and
/// non-SGR controls delimit matching; bytes inside OSC/DCS payload never do.
pub(crate) fn last_record_boundary(input: &[u8]) -> Option<usize> {
    if !input.iter().copied().any(is_control_starter) {
        return input
            .iter()
            .rposition(|byte| matches!(byte, b'\r' | b'\n' | 0x0b | 0x0c))
            .map(|position| position + 1);
    }

    let tokenized = Tokenized::new(input);
    tokenized
        .tokens
        .iter()
        .rev()
        .find_map(|token| match token.kind {
            TokenKind::Text => input[token.raw_start..token.raw_end]
                .iter()
                .rposition(|byte| matches!(byte, b'\r' | b'\n' | 0x0b | 0x0c))
                .map(|offset| token.raw_start + offset + 1),
            TokenKind::Control {
                sgr: false,
                complete: true,
            } => Some(token.raw_end),
            TokenKind::Control { .. } => None,
        })
}

/// Move a desired raw boundary out of the middle of a terminal control.
pub(crate) fn safe_raw_boundary(input: &[u8], desired: usize) -> usize {
    if desired >= input.len() {
        return input.len();
    }
    let tokenized = Tokenized::new(input);
    for token in tokenized.tokens {
        if desired <= token.raw_start {
            return desired;
        }
        if desired < token.raw_end {
            return match token.kind {
                TokenKind::Text => desired,
                TokenKind::Control { .. } => token.raw_start,
            };
        }
    }
    desired
}

/// Existing terminal rendition state needed to restore a highlighted span.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct SgrState {
    attrs: u16,
    fg: Option<RenderedColor>,
    bg: Option<RenderedColor>,
    underline: UnderlineState,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum UnderlineState {
    #[default]
    Off,
    Single,
    /// Legacy double-underline spelling, retained instead of canonicalized.
    Legacy21,
    /// Modern `4:<variant>` underline spelling.
    Colon(u16),
}

impl SgrState {
    pub(crate) fn update(&mut self, sequence: &[u8]) {
        let body = if sequence.starts_with(b"\x1b[") && sequence.ends_with(b"m") {
            &sequence[2..sequence.len() - 1]
        } else if sequence.starts_with(&[0x9b]) && sequence.ends_with(b"m") {
            &sequence[1..sequence.len() - 1]
        } else {
            return;
        };
        let parameters: Vec<&[u8]> = body.split(|byte| *byte == b';').collect();

        let mut cursor = 0;
        while cursor < parameters.len() {
            let section = parameters[cursor];
            let Some(code) = top_level_parameter(section) else {
                cursor += 1;
                continue;
            };
            match code {
                0 => *self = Self::default(),
                1 => self.attrs |= BOLD,
                2 => self.attrs |= DIM,
                3 => self.attrs |= ITALIC,
                4 => self.set_underline(underline_state(section)),
                21 => self.set_underline(UnderlineState::Legacy21),
                5 | 6 => self.attrs |= BLINK,
                7 => self.attrs |= INVERT,
                8 => self.attrs |= HIDDEN,
                9 => self.attrs |= STRIKE,
                22 => self.attrs &= !(BOLD | DIM),
                23 => self.attrs &= !ITALIC,
                24 => self.set_underline(UnderlineState::Off),
                25 => self.attrs &= !BLINK,
                27 => self.attrs &= !INVERT,
                28 => self.attrs &= !HIDDEN,
                29 => self.attrs &= !STRIKE,
                30..=37 => self.fg = Some(RenderedColor::Native((code - 30) as u8)),
                38 => {
                    if section.contains(&b':') {
                        if let Some(color) = extended_color_colon(section) {
                            self.fg = Some(color);
                        }
                    } else if let Some((color, consumed)) =
                        extended_color_semicolon(&parameters[cursor + 1..])
                    {
                        if let Some(color) = color {
                            self.fg = Some(color);
                        }
                        cursor += consumed;
                    }
                }
                39 => self.fg = None,
                40..=47 => self.bg = Some(RenderedColor::Native((code - 40) as u8)),
                48 => {
                    if section.contains(&b':') {
                        if let Some(color) = extended_color_colon(section) {
                            self.bg = Some(color);
                        }
                    } else if let Some((color, consumed)) =
                        extended_color_semicolon(&parameters[cursor + 1..])
                    {
                        if let Some(color) = color {
                            self.bg = Some(color);
                        }
                        cursor += consumed;
                    }
                }
                49 => self.bg = None,
                90..=97 => self.fg = Some(RenderedColor::Native((code - 90 + 8) as u8)),
                100..=107 => self.bg = Some(RenderedColor::Native((code - 100 + 8) as u8)),
                _ => {}
            }
            cursor += 1;
        }
    }

    pub(crate) fn emit_restore(self, overlay: Style, output: &mut Vec<u8>) {
        let mut parameters = SgrParameters::new();
        let mut colon_underline = None;
        if overlay.attrs & (BOLD | DIM) != 0 {
            parameters.push(22);
            if self.attrs & BOLD != 0 {
                parameters.push(1);
            }
            if self.attrs & DIM != 0 {
                parameters.push(2);
            }
        }
        if overlay.attrs & ITALIC != 0 {
            parameters.push(23);
            if self.attrs & ITALIC != 0 {
                parameters.push(3);
            }
        }
        if overlay.attrs & UNDERLINE != 0 {
            parameters.push(24);
            match self.underline {
                UnderlineState::Off => {}
                UnderlineState::Single => parameters.push(4),
                UnderlineState::Legacy21 => parameters.push(21),
                UnderlineState::Colon(variant) => colon_underline = Some(variant),
            }
        }
        for (flag, reset, enable) in [
            (BLINK, 25, 5),
            (INVERT, 27, 7),
            (HIDDEN, 28, 8),
            (STRIKE, 29, 9),
        ] {
            if overlay.attrs & flag != 0 {
                parameters.push(reset);
                if self.attrs & flag != 0 {
                    parameters.push(enable);
                }
            }
        }
        if overlay.fg.is_some() {
            if let Some(color) = self.fg {
                color.push_parameters(false, &mut parameters);
            } else {
                parameters.push(39);
            }
        }
        if overlay.bg.is_some() {
            if let Some(color) = self.bg {
                color.push_parameters(true, &mut parameters);
            } else {
                parameters.push(49);
            }
        }
        if !parameters.is_empty() {
            emit_sgr(parameters.as_slice(), output);
        }
        if let Some(variant) = colon_underline {
            emit_colon_underline(variant, output);
        }
    }

    fn set_underline(&mut self, underline: UnderlineState) {
        self.underline = underline;
        if underline == UnderlineState::Off {
            self.attrs &= !UNDERLINE;
        } else {
            self.attrs |= UNDERLINE;
        }
    }
}

fn underline_state(section: &[u8]) -> UnderlineState {
    let Some(colon) = section.iter().position(|byte| *byte == b':') else {
        return UnderlineState::Single;
    };
    match section[colon + 1..]
        .split(|byte| *byte == b':')
        .next()
        .and_then(parse_parameter)
    {
        Some(0) => UnderlineState::Off,
        Some(variant) => UnderlineState::Colon(variant),
        None => UnderlineState::Single,
    }
}

fn emit_colon_underline(mut variant: u16, output: &mut Vec<u8>) {
    output.extend_from_slice(b"\x1b[4:");
    let mut digits = [0_u8; 5];
    let mut cursor = digits.len();
    loop {
        cursor -= 1;
        digits[cursor] = b'0' + (variant % 10) as u8;
        variant /= 10;
        if variant == 0 {
            break;
        }
    }
    output.extend_from_slice(&digits[cursor..]);
    output.push(b'm');
}

fn top_level_parameter(section: &[u8]) -> Option<u16> {
    if section.is_empty() {
        return Some(0);
    }
    parse_parameter(section.split(|byte| *byte == b':').next()?)
}

/// Parse a semicolon-form 38/48 color and report how many following top-level
/// parameters belong to it. A recognized but malformed color is still consumed
/// so its components can never masquerade as unrelated rendition attributes.
fn extended_color_semicolon(parameters: &[&[u8]]) -> Option<(Option<RenderedColor>, usize)> {
    match parameters
        .first()
        .and_then(|value| parse_parameter(value))?
    {
        5 => {
            let consumed = parameters.len().min(2);
            let color = parameters
                .get(1)
                .and_then(|value| parse_parameter(value))
                .filter(|index| *index <= 255)
                .map(|index| RenderedColor::Indexed(index as u8));
            Some((color, consumed))
        }
        2 => {
            let has_color_space = parameters.get(1).is_some_and(|value| value.is_empty());
            let expected = if has_color_space { 5 } else { 4 };
            let consumed = parameters.len().min(expected);
            let rgb_start = if has_color_space { 2 } else { 1 };
            let component = |offset: usize| {
                parameters
                    .get(rgb_start + offset)
                    .and_then(|value| parse_parameter(value))
                    .filter(|value| *value <= 255)
            };
            let color = match (component(0), component(1), component(2)) {
                (Some(red), Some(green), Some(blue)) => {
                    Some(RenderedColor::Rgb(red as u8, green as u8, blue as u8))
                }
                _ => None,
            };
            Some((color, consumed))
        }
        _ => None,
    }
}

/// Parse one colon-form 38/48 section. Colon subparameters remain scoped to
/// their leading SGR code; e.g. `4:2` is underline, never underline plus dim.
fn extended_color_colon(section: &[u8]) -> Option<RenderedColor> {
    let parameters: Vec<_> = section
        .split(|byte| *byte == b':')
        .map(parse_parameter)
        .collect();
    match parameters.as_slice() {
        [Some(38 | 48), Some(5), Some(index), ..] if *index <= 255 => {
            Some(RenderedColor::Indexed(*index as u8))
        }
        [
            Some(38 | 48),
            Some(2),
            None | Some(0),
            Some(red),
            Some(green),
            Some(blue),
            ..,
        ] if *red <= 255 && *green <= 255 && *blue <= 255 => {
            Some(RenderedColor::Rgb(*red as u8, *green as u8, *blue as u8))
        }
        [Some(38 | 48), Some(2), Some(red), Some(green), Some(blue)]
            if *red <= 255 && *green <= 255 && *blue <= 255 =>
        {
            Some(RenderedColor::Rgb(*red as u8, *green as u8, *blue as u8))
        }
        _ => None,
    }
}

fn parse_parameter(parameter: &[u8]) -> Option<u16> {
    if parameter.is_empty() {
        None
    } else {
        std::str::from_utf8(parameter).ok()?.parse().ok()
    }
}
