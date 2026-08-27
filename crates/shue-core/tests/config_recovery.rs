use shue_core::{ColorDepth, Config, ConfigWarning};

fn highlight(config: &Config, input: &[u8]) -> Vec<u8> {
    let mut output = Vec::new();
    config.highlight(input, &mut output).expect("highlight");
    output
}

#[test]
fn malformed_document_returns_an_empty_config_and_warning() {
    let (config, warnings) =
        Config::from_yaml_recovering("rules:\n  - regex: [", ColorDepth::TrueColor);

    assert!(matches!(warnings.as_slice(), [ConfigWarning::Yaml { .. }]));
    assert_eq!(highlight(&config, b"plain bytes"), b"plain bytes");
}

#[test]
fn invalid_palette_entries_do_not_hide_valid_entries_or_rules() {
    let yaml = r##"
palette:
  good: '#010203'
  broken: definitely-not-a-color
  7: red
rules:
  - regex: good
    color: f.good
  - regex: broken
    color: f.broken
  - regex: native
    color: fg:red
"##;
    let (config, warnings) = Config::from_yaml_recovering(yaml, ColorDepth::TrueColor);

    assert_eq!(warnings.len(), 3);
    assert!(warnings.iter().any(|warning| matches!(
        warning,
        ConfigWarning::Palette { name, .. } if name == "broken"
    )));
    assert!(warnings.iter().any(|warning| matches!(
        warning,
        ConfigWarning::Palette { name, .. } if name.contains('7')
    )));
    assert!(warnings.iter().any(|warning| matches!(
        warning,
        ConfigWarning::Group { rule: 1, group, .. } if group == "0"
    )));
    assert_eq!(
        highlight(&config, b"good broken native"),
        b"\x1b[38;2;1;2;3mgood\x1b[39m broken \x1b[31mnative\x1b[39m"
    );
}

#[test]
fn invalid_rules_and_groups_are_skipped_at_the_narrowest_boundary() {
    let yaml = r#"
rules:
  - not-a-rule
  - regex: '('
    color: fg:red
  - regex: '(?<left>left):(?<right>right)'
    color:
      left: fg:green
      right: 42
      absent: fg:blue
  - regex: x
    color: fg:red
    exclusive: definitely
  - regex: x
    color: bg:blue
"#;
    let (config, warnings) = Config::from_yaml_recovering(yaml, ColorDepth::TrueColor);

    assert!(
        warnings
            .iter()
            .any(|warning| matches!(warning, ConfigWarning::Rule { rule: 0, .. }))
    );
    assert!(
        warnings
            .iter()
            .any(|warning| matches!(warning, ConfigWarning::Rule { rule: 1, .. }))
    );
    assert!(warnings.iter().any(|warning| matches!(
        warning,
        ConfigWarning::Group { rule: 2, group, .. } if group == "right"
    )));
    assert!(warnings.iter().any(|warning| matches!(
        warning,
        ConfigWarning::Group { rule: 2, group, .. } if group == "absent"
    )));
    let exclusive_warning = warnings
        .iter()
        .find(|warning| matches!(warning, ConfigWarning::Rule { rule: 3, .. }))
        .expect("invalid exclusive warning");
    assert!(exclusive_warning.to_string().starts_with("rule 4:"));

    assert_eq!(
        highlight(&config, b"left:right x"),
        b"\x1b[32mleft\x1b[39m:right \x1b[31;44mx\x1b[39;49m"
    );
}

#[test]
fn invalid_root_shapes_are_nonfatal() {
    let (config, warnings) =
        Config::from_yaml_recovering("palette: []\nrules: nope\n", ColorDepth::Ansi16);

    assert_eq!(warnings.len(), 2);
    assert!(
        warnings
            .iter()
            .all(|warning| matches!(warning, ConfigWarning::Root { .. }))
    );
    assert_eq!(highlight(&config, b"untouched"), b"untouched");
}

#[test]
fn empty_or_entirely_invalid_color_maps_explicitly_ignore_the_rule() {
    let yaml = r#"
rules:
  - regex: empty
    color: {}
  - regex: invalid
    color:
      missing: fg:red
  - regex: alive
    color: fg:green
"#;
    let (config, warnings) = Config::from_yaml_recovering(yaml, ColorDepth::Ansi16);

    assert!(warnings.iter().any(|warning| matches!(
        warning,
        ConfigWarning::Rule { rule: 0, reason } if reason.contains("no usable entries")
    )));
    assert!(warnings.iter().any(|warning| matches!(
        warning,
        ConfigWarning::Rule { rule: 1, reason } if reason.contains("no usable entries")
    )));
    assert_eq!(
        highlight(&config, b"empty invalid alive"),
        b"empty invalid \x1b[32malive\x1b[39m"
    );
}
