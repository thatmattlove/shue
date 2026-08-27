use shue_core::{ColorDepth, Config, StreamHighlighter};

fn render(yaml: &str, input: &[u8]) -> Vec<u8> {
    let config = Config::from_yaml(yaml, ColorDepth::TrueColor).expect("valid config");
    let mut output = Vec::new();
    config.highlight(input, &mut output).expect("valid match");
    output
}

fn strip_sgr(input: &[u8]) -> Vec<u8> {
    let mut output = Vec::new();
    let mut cursor = 0;
    while cursor < input.len() {
        if input[cursor..].starts_with(b"\x1b[") {
            if let Some(end) = input[cursor + 2..]
                .iter()
                .position(|byte| (0x40..=0x7e).contains(byte))
            {
                cursor += 2 + end + 1;
                continue;
            }
        }
        output.push(input[cursor]);
        cursor += 1;
    }
    output
}

#[test]
fn verifies_advanced_pcre_groups_overlap_and_exclusivity() {
    let lookbehind = r#"
rules:
  - regex: '(?<=ID: )\d+(?=;)'
    color: fg:yellow
"#;
    assert_eq!(render(lookbehind, b"ID: 123;"), b"ID: \x1b[33m123\x1b[39m;");

    // The overall lookahead is zero-width, but capture 1 is non-empty. PCRE2's
    // iterator advances safely and exposes overlapping captures at offsets 0/1.
    let overlapping_lookahead = r#"
rules:
  - regex: '(?=(\d{2}))'
    color:
      1: underline
"#;
    assert_eq!(render(overlapping_lookahead, b"123"), b"\x1b[4m123\x1b[24m");

    let backreferences = r#"
rules:
  - regex: '\b(\w+)\s+\1\b'
    color: bold
  - regex: '(?<word>go)-\k<word>'
    color:
      word: fg:green
"#;
    let backref_output = render(backreferences, b"echo echo go-go");
    assert_eq!(strip_sgr(&backref_output), b"echo echo go-go");
    assert!(backref_output.windows(3).any(|window| window == b"\x1b[1"));
    assert!(backref_output.windows(4).any(|window| window == b"\x1b[32"));

    let overlap = r#"
rules:
  - regex: bc
    color: fg:red
  - regex: cd
    color: bg:blue
"#;
    let overlap_output = render(overlap, b"abcd");
    assert_eq!(strip_sgr(&overlap_output), b"abcd");
    assert!(
        overlap_output
            .windows(6)
            .any(|window| window == b"\x1b[31;4")
    );

    // Exclusivity masks only rules that follow it. YAML order is observable:
    // an earlier overlapping rule has already produced its styling when a
    // later exclusive match is discovered.
    let exclusive_late = r#"
rules:
  - regex: abcd
    color: underline
  - regex: bc
    color: fg:red
    exclusive: true
"#;
    assert_eq!(
        render(exclusive_late, b"abcd"),
        b"\x1b[4ma\x1b[24m\x1b[4;31mbc\x1b[24;39m\x1b[4md\x1b[24m"
    );

    // The reverse order lets the exclusive span mask the later broad rule.
    let exclusive_early = r#"
rules:
  - regex: bc
    color: fg:red
    exclusive: true
  - regex: abcd
    color: underline
"#;
    assert_eq!(render(exclusive_early, b"abcd"), b"a\x1b[31mbc\x1b[39md");

    // Only colored capture spans become exclusive, not the uncolored remainder
    // of the regex's overall match.
    let exclusive_group = r#"
rules:
  - regex: '(ab)c'
    color:
      1: bold
    exclusive: true
  - regex: c
    color: fg:blue
"#;
    assert_eq!(
        render(exclusive_group, b"abc"),
        b"\x1b[1mab\x1b[22m\x1b[34mc\x1b[39m"
    );

    let invalid = "rules:\n  - regex: '(unterminated'\n    color: bold\n";
    assert!(Config::from_yaml(invalid, ColorDepth::TrueColor).is_err());

    // Runtime PCRE2 limits fail open: the offending rule is quarantined once,
    // its partial paints are discarded, and later safe rules keep the session
    // usable on this record and every subsequent record.
    let limited = r#"
rules:
  - regex: '(*LIMIT_MATCH=1)^(a+)+$'
    color: fg:red
  - regex: 'safe'
    color: fg:green
"#;
    let mut stream = StreamHighlighter::new(
        Config::from_yaml(limited, ColorDepth::TrueColor).expect("limit verb compiles"),
    );
    let hostile = b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa! safe\n";
    let mut fail_open = Vec::new();
    stream
        .push(hostile, &mut fail_open)
        .expect("runtime limit must not escape");
    assert_eq!(strip_sgr(&fail_open), hostile);
    assert!(fail_open.windows(4).any(|window| window == b"\x1b[32"));
    assert_eq!(stream.disabled_rule_count(), 1);
    let warnings = stream.take_runtime_warnings();
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].rule, 0);
    assert!(
        warnings[0]
            .message
            .to_ascii_lowercase()
            .contains("match limit")
    );

    fail_open.clear();
    stream
        .push(hostile, &mut fail_open)
        .expect("quarantined rule is skipped");
    assert_eq!(strip_sgr(&fail_open), hostile);
    assert!(fail_open.windows(4).any(|window| window == b"\x1b[32"));
    assert!(stream.take_runtime_warnings().is_empty());

    println!("advanced regex verification passed");
}
