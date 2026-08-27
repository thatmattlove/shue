use std::collections::{BTreeMap, HashMap};
use std::env;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use pcre2::bytes::{Regex, RegexBuilder};
use serde::Deserialize;
use serde_yaml::{Mapping, Value};
use thiserror::Error;

use crate::color::{Style, parse_palette_color, parse_style};
use crate::engine::{HighlightError, highlight_with_state};

/// Terminal color capability selected for rendered RGB values.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ColorDepth {
    /// The 16 theme-defined ANSI colors.
    Ansi16,
    /// The xterm 256-color palette.
    Ansi256,
    /// 24-bit RGB color.
    TrueColor,
}

/// Detect color capability from conventional terminal environment hints.
///
/// `COLORTERM=truecolor`/`24bit` takes priority. A `TERM` containing
/// `truecolor`, `24bit`, or `direct` selects truecolor, and one containing
/// `256color` selects ANSI-256. The conservative fallback is ANSI-16.
pub fn detect_color_depth() -> ColorDepth {
    let colorterm = env::var_os("COLORTERM")
        .map(|value| value.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if colorterm.contains("truecolor") || colorterm.contains("24bit") {
        return ColorDepth::TrueColor;
    }

    let term = env::var_os("TERM")
        .map(|value| value.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if term.contains("truecolor") || term.contains("24bit") || term.contains("direct") {
        ColorDepth::TrueColor
    } else if term.contains("256color") {
        ColorDepth::Ansi256
    } else {
        ColorDepth::Ansi16
    }
}

/// A precompiled highlighting configuration.
#[derive(Clone, Debug)]
pub struct Config {
    pub(crate) rules: Vec<Rule>,
    runtime_warnings: Arc<Mutex<Vec<RuntimeWarning>>>,
}

#[derive(Clone, Debug)]
pub(crate) struct Rule {
    pub(crate) regex: Regex,
    pub(crate) groups: Vec<GroupStyle>,
    pub(crate) exclusive: bool,
    pub(crate) source_index: usize,
    pub(crate) disabled: Arc<AtomicBool>,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct GroupStyle {
    pub(crate) group: usize,
    pub(crate) style: Style,
}

/// A once-only diagnostic for a regex quarantined after a runtime failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeWarning {
    /// Zero-based rule position in the YAML configuration.
    pub rule: usize,
    /// PCRE2's diagnostic text.
    pub message: String,
}

/// Configuration parsing and compilation failure.
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("invalid YAML configuration: {0}")]
    Yaml(#[from] serde_yaml::Error),
    #[error("configuration must contain a `rules` list")]
    MissingRules,
    #[error("palette color {name:?} is invalid: {reason}")]
    Palette { name: String, reason: String },
    #[error("rule {rule} has an invalid regular expression: {source}")]
    Regex {
        rule: usize,
        #[source]
        source: pcre2::Error,
    },
    #[error("rule {rule} has invalid color group {group:?}: {reason}")]
    Group {
        rule: usize,
        group: String,
        reason: String,
    },
    #[error("rule {rule}, group {group:?}, has an invalid color: {reason}")]
    Color {
        rule: usize,
        group: String,
        reason: String,
    },
}

/// A diagnostic emitted while recovering the usable parts of a configuration.
///
/// Rule indices are zero-based in the structured fields. Their human-readable
/// representation is one-based so diagnostics match editor line-item numbering.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ConfigWarning {
    /// The YAML document could not be parsed at all.
    Yaml { message: String },
    /// A top-level configuration field was missing or had the wrong shape.
    Root { message: String },
    /// One palette entry was ignored.
    Palette { name: String, reason: String },
    /// One rule, or a nonessential field on it, was ignored.
    Rule { rule: usize, reason: String },
    /// One color-group entry within an otherwise usable rule was ignored.
    Group {
        rule: usize,
        group: String,
        reason: String,
    },
}

impl fmt::Display for ConfigWarning {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Yaml { message } => write!(formatter, "invalid YAML configuration: {message}"),
            Self::Root { message } => write!(formatter, "invalid configuration: {message}"),
            Self::Palette { name, reason } => {
                write!(formatter, "palette entry {name:?} ignored: {reason}")
            }
            Self::Rule { rule, reason } => write!(formatter, "rule {}: {reason}", rule + 1),
            Self::Group {
                rule,
                group,
                reason,
            } => write!(
                formatter,
                "rule {}, group {group:?}, ignored: {reason}",
                rule + 1
            ),
        }
    }
}

impl std::error::Error for ConfigWarning {}

#[derive(Debug, Deserialize)]
struct RawConfig {
    #[serde(default)]
    palette: BTreeMap<String, String>,
    rules: Option<Vec<RawRule>>,
}

#[derive(Debug, Deserialize)]
struct RawRule {
    regex: String,
    color: Value,
    #[allow(dead_code)]
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    exclusive: bool,
}

impl Config {
    /// Parse ChromaTerm-compatible YAML and precompile all PCRE2 patterns.
    pub fn from_yaml(input: &str, depth: ColorDepth) -> Result<Self, ConfigError> {
        let raw: RawConfig = serde_yaml::from_str(input)?;
        let raw_rules = raw.rules.ok_or(ConfigError::MissingRules)?;

        let mut palette = HashMap::with_capacity(raw.palette.len());
        for (raw_name, raw_value) in raw.palette {
            let name = raw_name.trim().to_ascii_lowercase();
            if name.is_empty()
                || !name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
            {
                return Err(ConfigError::Palette {
                    name: raw_name,
                    reason: "names accept only ASCII alphanumerics, dashes, and underscores".into(),
                });
            }
            if palette.contains_key(&name) {
                return Err(ConfigError::Palette {
                    name: raw_name,
                    reason: "a case-insensitive palette name is defined more than once".into(),
                });
            }
            let color = parse_palette_color(&raw_value).map_err(|reason| ConfigError::Palette {
                name: raw_name,
                reason,
            })?;
            palette.insert(name, color);
        }

        let mut rules = Vec::with_capacity(raw_rules.len());
        for (rule_index, raw_rule) in raw_rules.into_iter().enumerate() {
            let mut builder = RegexBuilder::new();
            builder
                .jit_if_available(true)
                .max_jit_stack_size(Some(512 * 1024));
            let regex = builder
                .build(&raw_rule.regex)
                .map_err(|source| ConfigError::Regex {
                    rule: rule_index,
                    source,
                })?;

            let entries = color_entries(raw_rule.color, rule_index)?;
            let mut groups = Vec::with_capacity(entries.len());
            for (group_name, color) in entries {
                let group =
                    resolve_group(&regex, &group_name).map_err(|reason| ConfigError::Group {
                        rule: rule_index,
                        group: group_name.clone(),
                        reason,
                    })?;
                let style =
                    parse_style(&color, &palette, depth).map_err(|reason| ConfigError::Color {
                        rule: rule_index,
                        group: group_name.clone(),
                        reason,
                    })?;
                groups.push(GroupStyle { group, style });
            }
            // ChromaTerm resolves named groups to numeric indices and applies
            // group 0 before increasingly specific capture groups.
            groups.sort_by_key(|group| group.group);

            rules.push(Rule {
                regex,
                groups,
                exclusive: raw_rule.exclusive,
                source_index: rule_index,
                disabled: Arc::new(AtomicBool::new(false)),
            });
        }
        Ok(Self {
            rules,
            runtime_warnings: Arc::new(Mutex::new(Vec::new())),
        })
    }

    /// Parse and compile every usable part of a YAML configuration.
    ///
    /// Unlike [`Config::from_yaml`], this never returns an error. Invalid
    /// palette entries and rules are omitted, while invalid group-color entries
    /// are omitted from otherwise valid rules. A document-level YAML failure
    /// produces an empty configuration. Every discarded part is described by a
    /// structured warning returned alongside the configuration.
    pub fn from_yaml_recovering(input: &str, depth: ColorDepth) -> (Self, Vec<ConfigWarning>) {
        let document = match serde_yaml::from_str::<Value>(input) {
            Ok(document) => document,
            Err(source) => {
                return (
                    Self::empty(),
                    vec![ConfigWarning::Yaml {
                        message: source.to_string(),
                    }],
                );
            }
        };
        let Value::Mapping(root) = document else {
            return (
                Self::empty(),
                vec![ConfigWarning::Root {
                    message: format!("expected a mapping, got {}", yaml_kind(&document)),
                }],
            );
        };

        let mut warnings = Vec::new();
        let palette = recovering_palette(mapping_field(&root, "palette"), &mut warnings);
        let Some(raw_rules) = mapping_field(&root, "rules") else {
            warnings.push(ConfigWarning::Root {
                message: "configuration must contain a `rules` list".into(),
            });
            return (Self::empty(), warnings);
        };
        let Value::Sequence(raw_rules) = raw_rules else {
            warnings.push(ConfigWarning::Root {
                message: format!("`rules` must be a list, got {}", yaml_kind(raw_rules)),
            });
            return (Self::empty(), warnings);
        };

        let mut rules = Vec::with_capacity(raw_rules.len());
        for (rule_index, raw_rule) in raw_rules.iter().enumerate() {
            if let Some(rule) =
                recovering_rule(raw_rule, rule_index, &palette, depth, &mut warnings)
            {
                rules.push(rule);
            }
        }

        (
            Self {
                rules,
                runtime_warnings: Arc::new(Mutex::new(Vec::new())),
            },
            warnings,
        )
    }

    /// Highlight one complete byte slice, appending bytes to `output`.
    pub fn highlight(&self, input: &[u8], output: &mut Vec<u8>) -> Result<(), HighlightError> {
        let mut state = crate::ansi::SgrState::default();
        highlight_with_state(self, input, input.len(), output, &mut state)
    }

    /// Drain once-only runtime regex warnings accumulated while highlighting.
    ///
    /// A rule that reaches a PCRE2 runtime limit is disabled before its warning
    /// is queued, allowing a CLI to report the issue on stderr without ending
    /// the terminal session or writing diagnostic bytes into highlighted data.
    pub fn take_runtime_warnings(&self) -> Vec<RuntimeWarning> {
        let mut warnings = self
            .runtime_warnings
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        std::mem::take(&mut *warnings)
    }

    /// Count rules quarantined by runtime PCRE2 failures.
    pub fn disabled_rule_count(&self) -> usize {
        self.rules
            .iter()
            .filter(|rule| rule.disabled.load(Ordering::Acquire))
            .count()
    }

    pub(crate) fn quarantine_rule(&self, rule_index: usize, source: &pcre2::Error) {
        let rule = &self.rules[rule_index];
        if rule.disabled.swap(true, Ordering::AcqRel) {
            return;
        }
        let mut warnings = self
            .runtime_warnings
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        warnings.push(RuntimeWarning {
            rule: rule.source_index,
            message: source.to_string(),
        });
    }

    fn empty() -> Self {
        Self {
            rules: Vec::new(),
            runtime_warnings: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

fn recovering_palette(
    raw_palette: Option<&Value>,
    warnings: &mut Vec<ConfigWarning>,
) -> HashMap<String, crate::color::BaseColor> {
    let Some(raw_palette) = raw_palette else {
        return HashMap::new();
    };
    let Value::Mapping(raw_palette) = raw_palette else {
        warnings.push(ConfigWarning::Root {
            message: format!(
                "`palette` must be a mapping, got {}; ignoring it",
                yaml_kind(raw_palette)
            ),
        });
        return HashMap::new();
    };

    let mut palette = HashMap::with_capacity(raw_palette.len());
    for (raw_name, raw_value) in raw_palette {
        let name = match raw_name {
            Value::String(name) => name,
            other => {
                warnings.push(ConfigWarning::Palette {
                    name: format!("{other:?}"),
                    reason: "names must be strings".into(),
                });
                continue;
            }
        };
        let normalized = name.trim().to_ascii_lowercase();
        if normalized.is_empty()
            || !normalized
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        {
            warnings.push(ConfigWarning::Palette {
                name: name.clone(),
                reason: "names accept only ASCII alphanumerics, dashes, and underscores".into(),
            });
            continue;
        }
        if palette.contains_key(&normalized) {
            warnings.push(ConfigWarning::Palette {
                name: name.clone(),
                reason: "a case-insensitive palette name is defined more than once".into(),
            });
            continue;
        }
        let Value::String(raw_value) = raw_value else {
            warnings.push(ConfigWarning::Palette {
                name: name.clone(),
                reason: format!("expected a string, got {}", yaml_kind(raw_value)),
            });
            continue;
        };
        match parse_palette_color(raw_value) {
            Ok(color) => {
                palette.insert(normalized, color);
            }
            Err(reason) => warnings.push(ConfigWarning::Palette {
                name: name.clone(),
                reason,
            }),
        }
    }
    palette
}

fn recovering_rule(
    raw_rule: &Value,
    rule_index: usize,
    palette: &HashMap<String, crate::color::BaseColor>,
    depth: ColorDepth,
    warnings: &mut Vec<ConfigWarning>,
) -> Option<Rule> {
    let Value::Mapping(raw_rule) = raw_rule else {
        warnings.push(ConfigWarning::Rule {
            rule: rule_index,
            reason: format!("expected a mapping, got {}", yaml_kind(raw_rule)),
        });
        return None;
    };

    let regex_source = match mapping_field(raw_rule, "regex") {
        Some(Value::String(regex)) => regex,
        Some(other) => {
            warnings.push(ConfigWarning::Rule {
                rule: rule_index,
                reason: format!("`regex` must be a string, got {}", yaml_kind(other)),
            });
            return None;
        }
        None => {
            warnings.push(ConfigWarning::Rule {
                rule: rule_index,
                reason: "missing `regex`".into(),
            });
            return None;
        }
    };

    let mut builder = RegexBuilder::new();
    builder
        .jit_if_available(true)
        .max_jit_stack_size(Some(512 * 1024));
    let regex = match builder.build(regex_source) {
        Ok(regex) => regex,
        Err(source) => {
            warnings.push(ConfigWarning::Rule {
                rule: rule_index,
                reason: format!("invalid regular expression: {source}"),
            });
            return None;
        }
    };

    if let Some(description) = mapping_field(raw_rule, "description") {
        if !matches!(description, Value::String(_) | Value::Null) {
            warnings.push(ConfigWarning::Rule {
                rule: rule_index,
                reason: format!("invalid `description` ({}) ignored", yaml_kind(description)),
            });
        }
    }

    let exclusive = match mapping_field(raw_rule, "exclusive") {
        None => false,
        Some(Value::Bool(exclusive)) => *exclusive,
        Some(other) => {
            warnings.push(ConfigWarning::Rule {
                rule: rule_index,
                reason: format!(
                    "invalid `exclusive` (expected a boolean, got {}); using false",
                    yaml_kind(other)
                ),
            });
            false
        }
    };

    let raw_color = match mapping_field(raw_rule, "color") {
        Some(color) => color,
        None => {
            warnings.push(ConfigWarning::Rule {
                rule: rule_index,
                reason: "missing `color`".into(),
            });
            return None;
        }
    };
    let mut groups = recovering_groups(raw_color, &regex, rule_index, palette, depth, warnings)?;
    groups.sort_by_key(|group| group.group);

    Some(Rule {
        regex,
        groups,
        exclusive,
        source_index: rule_index,
        disabled: Arc::new(AtomicBool::new(false)),
    })
}

fn recovering_groups(
    raw_color: &Value,
    regex: &Regex,
    rule_index: usize,
    palette: &HashMap<String, crate::color::BaseColor>,
    depth: ColorDepth,
    warnings: &mut Vec<ConfigWarning>,
) -> Option<Vec<GroupStyle>> {
    match raw_color {
        Value::String(color) => {
            let style = match parse_style(color, palette, depth) {
                Ok(style) => style,
                Err(reason) => {
                    warnings.push(ConfigWarning::Group {
                        rule: rule_index,
                        group: "0".into(),
                        reason,
                    });
                    return None;
                }
            };
            Some(vec![GroupStyle { group: 0, style }])
        }
        Value::Mapping(mapping) => {
            let mut groups = Vec::with_capacity(mapping.len());
            for (raw_group, raw_color) in mapping {
                let group_name = match raw_group {
                    Value::String(group) => group.clone(),
                    Value::Number(group) if group.as_u64().is_some() => group.to_string(),
                    other => {
                        warnings.push(ConfigWarning::Group {
                            rule: rule_index,
                            group: format!("{other:?}"),
                            reason: "group keys must be non-negative integers or capture names"
                                .into(),
                        });
                        continue;
                    }
                };
                let Value::String(color) = raw_color else {
                    warnings.push(ConfigWarning::Group {
                        rule: rule_index,
                        group: group_name,
                        reason: format!("expected a string, got {}", yaml_kind(raw_color)),
                    });
                    continue;
                };
                let group = match resolve_group(regex, &group_name) {
                    Ok(group) => group,
                    Err(reason) => {
                        warnings.push(ConfigWarning::Group {
                            rule: rule_index,
                            group: group_name,
                            reason,
                        });
                        continue;
                    }
                };
                let style = match parse_style(color, palette, depth) {
                    Ok(style) => style,
                    Err(reason) => {
                        warnings.push(ConfigWarning::Group {
                            rule: rule_index,
                            group: group_name,
                            reason,
                        });
                        continue;
                    }
                };
                groups.push(GroupStyle { group, style });
            }
            if groups.is_empty() {
                warnings.push(ConfigWarning::Rule {
                    rule: rule_index,
                    reason: "ignored because its color map contains no usable entries".into(),
                });
                None
            } else {
                Some(groups)
            }
        }
        other => {
            warnings.push(ConfigWarning::Rule {
                rule: rule_index,
                reason: format!(
                    "`color` must be a string or group-to-string map, got {}",
                    yaml_kind(other)
                ),
            });
            None
        }
    }
}

fn mapping_field<'a>(mapping: &'a Mapping, name: &str) -> Option<&'a Value> {
    mapping.iter().find_map(|(key, value)| match key {
        Value::String(key) if key == name => Some(value),
        _ => None,
    })
}

fn color_entries(value: Value, rule: usize) -> Result<Vec<(String, String)>, ConfigError> {
    match value {
        Value::String(color) => Ok(vec![("0".into(), color)]),
        Value::Mapping(mapping) => mapping_entries(mapping, rule),
        other => Err(ConfigError::Color {
            rule,
            group: "0".into(),
            reason: format!(
                "expected a string or group-to-string map, got {}",
                yaml_kind(&other)
            ),
        }),
    }
}

fn mapping_entries(mapping: Mapping, rule: usize) -> Result<Vec<(String, String)>, ConfigError> {
    let mut entries = Vec::with_capacity(mapping.len());
    for (key, value) in mapping {
        let group = match key {
            Value::String(value) => value,
            Value::Number(value) if value.as_u64().is_some() => value.to_string(),
            other => {
                return Err(ConfigError::Group {
                    rule,
                    group: format!("{other:?}"),
                    reason: "group keys must be non-negative integers or capture names".into(),
                });
            }
        };
        let color = match value {
            Value::String(value) => value,
            other => {
                return Err(ConfigError::Color {
                    rule,
                    group,
                    reason: format!("expected a string, got {}", yaml_kind(&other)),
                });
            }
        };
        entries.push((group, color));
    }
    Ok(entries)
}

fn resolve_group(regex: &Regex, group: &str) -> Result<usize, String> {
    if !group.is_empty() && group.bytes().all(|byte| byte.is_ascii_digit()) {
        let index = group
            .parse::<usize>()
            .map_err(|_| "capture index is too large".to_string())?;
        if index < regex.captures_len() {
            return Ok(index);
        }
        return Err(format!(
            "pattern exposes groups 0 through {}",
            regex.captures_len().saturating_sub(1)
        ));
    }

    regex
        .capture_names()
        .iter()
        .position(|name| name.as_deref() == Some(group))
        .ok_or_else(|| "named capture does not exist in the pattern".into())
}

fn yaml_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Sequence(_) => "sequence",
        Value::Mapping(_) => "mapping",
        Value::Tagged(_) => "tagged value",
    }
}
