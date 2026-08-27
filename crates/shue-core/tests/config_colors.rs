use shue_core::{ColorDepth, Config};

fn render(yaml: &str, depth: ColorDepth, input: &[u8]) -> Vec<u8> {
    let config = Config::from_yaml(yaml, depth).expect("configuration should compile");
    let mut output = Vec::new();
    config
        .highlight(input, &mut output)
        .expect("highlighting should succeed");
    output
}

#[test]
fn verifies_chromaterm_config_and_every_color_family() {
    let native = r#"
rules:
  - description: terminal theme colors
    regex: native
    color: fg:red bg:bright-blue
"#;
    assert_eq!(
        render(native, ColorDepth::TrueColor, b"native"),
        b"\x1b[31;104mnative\x1b[39;49m"
    );
    // Theme-native names deliberately stay ANSI-native at every selected depth.
    assert_eq!(
        render(native, ColorDepth::Ansi256, b"native"),
        b"\x1b[31;104mnative\x1b[39;49m"
    );

    let rgb = r##"
rules:
  - regex: hex
    color: f#112233
  - regex: rgb
    color: bg:rgb(4, 5, 6)
"##;
    assert_eq!(
        render(rgb, ColorDepth::TrueColor, b"hex rgb"),
        b"\x1b[38;2;17;34;51mhex\x1b[39m \x1b[48;2;4;5;6mrgb\x1b[49m"
    );

    let palette = r##"
palette:
  accent: '#abcdef'
rules:
  - regex: palette
    color: f.accent bold
"##;
    assert_eq!(
        render(palette, ColorDepth::TrueColor, b"palette"),
        b"\x1b[1;38;2;171;205;239mpalette\x1b[22;39m"
    );

    let all_styles = r#"
rules:
  - regex: styled
    color: bold dim italic underline blink invert hidden strike
"#;
    assert_eq!(
        render(all_styles, ColorDepth::Ansi16, b"styled"),
        b"\x1b[1;2;3;4;5;7;8;9mstyled\x1b[22;23;24;25;27;28;29m"
    );

    // RGB degradation follows ChromaTerm's stable xterm cube/gray algorithm.
    let red = r##"
rules:
  - regex: red
    color: fg:#ff0000
"##;
    assert_eq!(
        render(red, ColorDepth::Ansi256, b"red"),
        b"\x1b[38;5;196mred\x1b[39m"
    );
    assert_eq!(
        render(red, ColorDepth::Ansi16, b"red"),
        b"\x1b[91mred\x1b[39m"
    );

    let groups = r#"
rules:
  - regex: '(?P<left>left):(right)'
    color:
      left: fg:red
      2: fg:blue
"#;
    assert_eq!(
        render(groups, ColorDepth::TrueColor, b"left:right"),
        b"\x1b[31mleft\x1b[39m:\x1b[34mright\x1b[39m"
    );

    assert!(Config::from_yaml("palette: {}", ColorDepth::TrueColor).is_err());
    assert!(
        Config::from_yaml(
            "rules:\n  - regex: '(x)'\n    color:\n      2: bold\n",
            ColorDepth::TrueColor,
        )
        .is_err()
    );
    assert!(
        Config::from_yaml(
            "rules:\n  - regex: x\n    color: 'fg:red fg:blue'\n",
            ColorDepth::TrueColor,
        )
        .is_err()
    );
    assert!(
        Config::from_yaml(
            "rules:\n  - regex: x\n    color: 'f.red'\n",
            ColorDepth::TrueColor,
        )
        .is_err(),
        "legacy f.name syntax must resolve a declared palette entry"
    );
    assert!(
        Config::from_yaml(
            "palette:\n  Accent: '#000000'\n  accent: '#ffffff'\nrules: []\n",
            ColorDepth::TrueColor,
        )
        .is_err(),
        "palette lookup is case-insensitive and collisions are rejected"
    );

    println!("core color and config verification passed");
}
