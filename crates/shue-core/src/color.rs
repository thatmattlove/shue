use std::collections::HashMap;

use crate::config::ColorDepth;

pub(crate) const BOLD: u16 = 1 << 0;
pub(crate) const DIM: u16 = 1 << 1;
pub(crate) const ITALIC: u16 = 1 << 2;
pub(crate) const UNDERLINE: u16 = 1 << 3;
pub(crate) const BLINK: u16 = 1 << 4;
pub(crate) const INVERT: u16 = 1 << 5;
pub(crate) const HIDDEN: u16 = 1 << 6;
pub(crate) const STRIKE: u16 = 1 << 7;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BaseColor {
    Rgb(u8, u8, u8),
    /// Theme-native ANSI index (0..=15).
    Native(u8),
    Default,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RenderedColor {
    Native(u8),
    Indexed(u8),
    Rgb(u8, u8, u8),
    Default,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct Style {
    pub(crate) fg: Option<RenderedColor>,
    pub(crate) bg: Option<RenderedColor>,
    pub(crate) attrs: u16,
}

pub(crate) struct SgrParameters {
    values: [u16; 32],
    len: usize,
}

impl SgrParameters {
    pub(crate) fn new() -> Self {
        Self {
            values: [0; 32],
            len: 0,
        }
    }

    pub(crate) fn push(&mut self, value: u16) {
        self.values[self.len] = value;
        self.len += 1;
    }

    pub(crate) fn extend(&mut self, values: &[u16]) {
        let end = self.len + values.len();
        self.values[self.len..end].copy_from_slice(values);
        self.len = end;
    }

    pub(crate) fn as_slice(&self) -> &[u16] {
        &self.values[..self.len]
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl Style {
    pub(crate) fn overlay(&mut self, newer: Style) {
        if newer.fg.is_some() {
            self.fg = newer.fg;
        }
        if newer.bg.is_some() {
            self.bg = newer.bg;
        }
        self.attrs |= newer.attrs;
    }

    pub(crate) fn is_empty(self) -> bool {
        self.fg.is_none() && self.bg.is_none() && self.attrs == 0
    }

    pub(crate) fn emit_open(self, output: &mut Vec<u8>) {
        if self.is_empty() {
            return;
        }
        let mut parameters = SgrParameters::new();
        for (flag, code) in [
            (BOLD, 1),
            (DIM, 2),
            (ITALIC, 3),
            (UNDERLINE, 4),
            (BLINK, 5),
            (INVERT, 7),
            (HIDDEN, 8),
            (STRIKE, 9),
        ] {
            if self.attrs & flag != 0 {
                parameters.push(code);
            }
        }
        if let Some(color) = self.fg {
            color.push_parameters(false, &mut parameters);
        }
        if let Some(color) = self.bg {
            color.push_parameters(true, &mut parameters);
        }
        emit_sgr(parameters.as_slice(), output);
    }
}

impl RenderedColor {
    pub(crate) fn push_parameters(self, background: bool, parameters: &mut SgrParameters) {
        match self {
            Self::Native(index) => {
                let code = match (background, index) {
                    (false, 0..=7) => 30 + u16::from(index),
                    (false, 8..=15) => 90 + u16::from(index - 8),
                    (true, 0..=7) => 40 + u16::from(index),
                    (true, 8..=15) => 100 + u16::from(index - 8),
                    _ => unreachable!("ANSI native color index is always 0..=15"),
                };
                parameters.push(code);
            }
            Self::Indexed(index) => {
                parameters.extend(&[if background { 48 } else { 38 }, 5, u16::from(index)]);
            }
            Self::Rgb(red, green, blue) => {
                parameters.extend(&[
                    if background { 48 } else { 38 },
                    2,
                    u16::from(red),
                    u16::from(green),
                    u16::from(blue),
                ]);
            }
            Self::Default => parameters.push(if background { 49 } else { 39 }),
        }
    }
}

pub(crate) fn emit_sgr(parameters: &[u16], output: &mut Vec<u8>) {
    output.extend_from_slice(b"\x1b[");
    for (index, parameter) in parameters.iter().copied().enumerate() {
        if index != 0 {
            output.push(b';');
        }
        push_decimal(parameter, output);
    }
    output.push(b'm');
}

fn push_decimal(mut value: u16, output: &mut Vec<u8>) {
    let mut digits = [0_u8; 5];
    let mut cursor = digits.len();
    loop {
        cursor -= 1;
        digits[cursor] = b'0' + (value % 10) as u8;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    output.extend_from_slice(&digits[cursor..]);
}

pub(crate) fn parse_palette_color(input: &str) -> Result<BaseColor, String> {
    let value = input.trim().to_ascii_lowercase();
    if let Some(rgb) = parse_hex(&value) {
        return Ok(rgb);
    }
    if value.starts_with("rgb") {
        return parse_rgb(&value);
    }
    parse_native(&value).ok_or_else(|| "expected #rrggbb, rgb(r,g,b), or an ANSI color name".into())
}

pub(crate) fn parse_style(
    input: &str,
    palette: &HashMap<String, BaseColor>,
    depth: ColorDepth,
) -> Result<Style, String> {
    let tokens = split_tokens(input)?;
    if tokens.is_empty() {
        return Err("color cannot be empty".into());
    }

    let mut style = Style::default();
    for token in tokens {
        let token = token.to_ascii_lowercase();
        let attr = match token.as_str() {
            "bold" => Some(BOLD),
            "dim" | "faint" => Some(DIM),
            "italic" => Some(ITALIC),
            "underline" => Some(UNDERLINE),
            "blink" => Some(BLINK),
            "invert" | "inverse" | "reverse" => Some(INVERT),
            "hidden" | "conceal" => Some(HIDDEN),
            "strike" | "strikethrough" => Some(STRIKE),
            _ => None,
        };
        if let Some(attr) = attr {
            style.attrs |= attr;
            continue;
        }

        let (background, value, palette_only) = if let Some(value) = token.strip_prefix("fg:") {
            (false, value, false)
        } else if let Some(value) = token.strip_prefix("bg:") {
            (true, value, false)
        } else if token.starts_with("f#") {
            (false, token.strip_prefix('f').expect("known prefix"), false)
        } else if token.starts_with("b#") {
            (true, token.strip_prefix('b').expect("known prefix"), false)
        } else if let Some(value) = token.strip_prefix("f.") {
            (false, value, true)
        } else if let Some(value) = token.strip_prefix("b.") {
            (true, value, true)
        } else {
            return Err(format!("unrecognized color or style token {token:?}"));
        };

        let base = if palette_only {
            palette
                .get(value)
                .copied()
                .ok_or_else(|| format!("palette color {value:?} does not exist"))?
        } else if let Some(color) = palette.get(value).copied() {
            color
        } else if let Some(color) = parse_hex(value) {
            color
        } else if value.starts_with("rgb") {
            parse_rgb(value)?
        } else {
            parse_native(value).ok_or_else(|| format!("unknown ANSI color name {value:?}"))?
        };
        let rendered = render_color(base, depth);
        let slot = if background {
            &mut style.bg
        } else {
            &mut style.fg
        };
        if slot.replace(rendered).is_some() {
            return Err(format!(
                "color accepts only one {} value",
                if background {
                    "background"
                } else {
                    "foreground"
                }
            ));
        }
    }

    if style.is_empty() {
        Err("color must contain at least one foreground, background, or style".into())
    } else {
        Ok(style)
    }
}

fn split_tokens(input: &str) -> Result<Vec<&str>, String> {
    let mut tokens = Vec::new();
    let mut start = None;
    let mut parentheses = 0_u8;
    for (index, character) in input.char_indices() {
        if character.is_ascii_whitespace() && parentheses == 0 {
            if let Some(token_start) = start.take() {
                tokens.push(&input[token_start..index]);
            }
            continue;
        }
        start.get_or_insert(index);
        match character {
            '(' => parentheses = parentheses.saturating_add(1),
            ')' if parentheses == 0 => return Err("unbalanced `)` in RGB color".into()),
            ')' => parentheses -= 1,
            _ => {}
        }
    }
    if parentheses != 0 {
        return Err("unclosed `(` in RGB color".into());
    }
    if let Some(token_start) = start {
        tokens.push(&input[token_start..]);
    }
    Ok(tokens)
}

fn parse_hex(value: &str) -> Option<BaseColor> {
    let hex = value.strip_prefix('#')?;
    if hex.len() != 6 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    Some(BaseColor::Rgb(
        u8::from_str_radix(&hex[0..2], 16).ok()?,
        u8::from_str_radix(&hex[2..4], 16).ok()?,
        u8::from_str_radix(&hex[4..6], 16).ok()?,
    ))
}

fn parse_rgb(value: &str) -> Result<BaseColor, String> {
    let body = value
        .strip_prefix("rgb(")
        .and_then(|value| value.strip_suffix(')'))
        .ok_or_else(|| format!("invalid RGB color {value:?}; expected rgb(r,g,b)"))?;
    let components: Vec<_> = body.split(',').map(str::trim).collect();
    if components.len() != 3 {
        return Err(format!(
            "invalid RGB color {value:?}; expected exactly three components"
        ));
    }
    let parse = |component: &str| {
        component.parse::<u8>().map_err(|_| {
            format!("RGB component {component:?} is not an integer from 0 through 255")
        })
    };
    Ok(BaseColor::Rgb(
        parse(components[0])?,
        parse(components[1])?,
        parse(components[2])?,
    ))
}

fn parse_native(value: &str) -> Option<BaseColor> {
    let index = match value {
        "black" => 0,
        "red" => 1,
        "green" => 2,
        "yellow" => 3,
        "blue" => 4,
        "magenta" | "purple" => 5,
        "cyan" => 6,
        "white" => 7,
        "bright-black" | "bright_black" | "gray" | "grey" | "dark-gray" | "dark-grey" => 8,
        "bright-red" | "bright_red" | "light-red" => 9,
        "bright-green" | "bright_green" | "light-green" => 10,
        "bright-yellow" | "bright_yellow" | "light-yellow" => 11,
        "bright-blue" | "bright_blue" | "light-blue" => 12,
        "bright-magenta" | "bright_magenta" | "bright-purple" | "light-magenta" => 13,
        "bright-cyan" | "bright_cyan" | "light-cyan" => 14,
        "bright-white" | "bright_white" | "light-white" => 15,
        "default" | "normal" => return Some(BaseColor::Default),
        _ => return None,
    };
    Some(BaseColor::Native(index))
}

fn render_color(color: BaseColor, depth: ColorDepth) -> RenderedColor {
    match color {
        BaseColor::Native(index) => RenderedColor::Native(index),
        BaseColor::Default => RenderedColor::Default,
        BaseColor::Rgb(red, green, blue) => match depth {
            ColorDepth::TrueColor => RenderedColor::Rgb(red, green, blue),
            ColorDepth::Ansi256 => RenderedColor::Indexed(rgb_to_ansi256(red, green, blue)),
            ColorDepth::Ansi16 => RenderedColor::Native(rgb_to_ansi16(red, green, blue)),
        },
    }
}

fn rgb_to_ansi256(red: u8, green: u8, blue: u8) -> u8 {
    const CUBE: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let nearest = |value: u8, steps: &[u8]| -> usize {
        let mut best = 0;
        let mut best_distance = u16::MAX;
        for (index, step) in steps.iter().copied().enumerate() {
            let distance = (i16::from(step) - i16::from(value)).unsigned_abs();
            if distance < best_distance {
                best = index;
                best_distance = distance;
            }
        }
        best
    };
    let cube_indices = [
        nearest(red, &CUBE),
        nearest(green, &CUBE),
        nearest(blue, &CUBE),
    ];
    let squared_distance = |candidate: [u8; 3]| -> u32 {
        [red, green, blue]
            .into_iter()
            .zip(candidate)
            .map(|(actual, candidate)| {
                let difference = i32::from(candidate) - i32::from(actual);
                (difference * difference) as u32
            })
            .sum()
    };
    let cube_distance = squared_distance([
        CUBE[cube_indices[0]],
        CUBE[cube_indices[1]],
        CUBE[cube_indices[2]],
    ]);

    let average = ((u16::from(red) + u16::from(green) + u16::from(blue)) / 3) as u8;
    let mut gray_steps = [0_u8; 24];
    for (index, value) in gray_steps.iter_mut().enumerate() {
        *value = 8 + (index as u8 * 10);
    }
    let gray_index = nearest(average, &gray_steps);
    let gray = gray_steps[gray_index];
    let gray_distance = squared_distance([gray, gray, gray]);
    if gray_distance < cube_distance {
        232 + gray_index as u8
    } else {
        16 + (36 * cube_indices[0] as u8) + (6 * cube_indices[1] as u8) + cube_indices[2] as u8
    }
}

fn rgb_to_ansi16(red: u8, green: u8, blue: u8) -> u8 {
    const ANSI16: [[u8; 3]; 16] = [
        [0, 0, 0],
        [128, 0, 0],
        [0, 128, 0],
        [128, 128, 0],
        [0, 0, 128],
        [128, 0, 128],
        [0, 128, 128],
        [192, 192, 192],
        [128, 128, 128],
        [255, 0, 0],
        [0, 255, 0],
        [255, 255, 0],
        [0, 0, 255],
        [255, 0, 255],
        [0, 255, 255],
        [255, 255, 255],
    ];
    let mut best = 0_u8;
    let mut best_distance = u32::MAX;
    for (index, candidate) in ANSI16.iter().copied().enumerate() {
        let distance = [red, green, blue]
            .into_iter()
            .zip(candidate)
            .map(|(actual, candidate)| {
                let difference = i32::from(candidate) - i32::from(actual);
                (difference * difference) as u32
            })
            .sum();
        if distance < best_distance {
            best = index as u8;
            best_distance = distance;
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chromaterm_256_conversion_is_stable() {
        assert_eq!(rgb_to_ansi256(255, 0, 0), 196);
        assert_eq!(rgb_to_ansi256(18, 49, 35), 235);
        assert_eq!(rgb_to_ansi256(171, 205, 239), 153);
    }
}
