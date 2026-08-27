use shue_core::{ColorDepth, Config, MAX_PENDING_BYTES, StreamHighlighter};

fn config(regex: &str, color: &str) -> Config {
    let yaml = format!("rules:\n  - regex: '{regex}'\n    color: '{color}'\n");
    Config::from_yaml(&yaml, ColorDepth::TrueColor).expect("valid test configuration")
}

fn render(config: &Config, input: &[u8]) -> Vec<u8> {
    let mut output = Vec::new();
    config.highlight(input, &mut output).expect("highlight");
    output
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

#[test]
fn verifies_ansi_binary_and_bounded_stream_behavior() {
    let red = config("error", "fg:red");
    assert_eq!(
        render(&red, b"\x1b[34mbefore error after\x1b[0m"),
        b"\x1b[34mbefore \x1b[31merror\x1b[34m after\x1b[0m"
    );

    // Original SGR changes inside a highlighted match remain byte-for-byte and
    // become the base state restored around/after the overlay.
    assert_eq!(
        render(&red, b"er\x1b[32mro\x1b[0mr"),
        b"\x1b[31mer\x1b[39m\x1b[32m\x1b[31mro\x1b[32m\x1b[0m\x1b[31mr\x1b[39m"
    );

    // Modern ISO colon-form truecolor is parsed for accurate base restoration.
    assert_eq!(
        render(&red, b"\x1b[38:2::1:2:3merror"),
        b"\x1b[38:2::1:2:3m\x1b[31merror\x1b[38;2;1;2;3m"
    );
    assert_eq!(
        render(&red, b"\x1b[38:2:0:4:5:6merror"),
        b"\x1b[38:2:0:4:5:6m\x1b[31merror\x1b[38;2;4;5;6m"
    );

    // Colon subparameters belong only to their leading SGR parameter. Double
    // and curly underline must never be misread as dim or italic state.
    assert_eq!(
        render(&config("error", "dim"), b"\x1b[4:2merror"),
        b"\x1b[4:2m\x1b[2merror\x1b[22m"
    );
    assert_eq!(
        render(&config("error", "italic"), b"\x1b[4:3merror"),
        b"\x1b[4:3m\x1b[3merror\x1b[23m"
    );
    assert_eq!(
        render(&config("error", "underline"), b"\x1b[4:2merror"),
        b"\x1b[4:2m\x1b[4merror\x1b[24m\x1b[4:2m"
    );
    assert_eq!(
        render(&config("error", "underline"), b"\x1b[21merror"),
        b"\x1b[21m\x1b[4merror\x1b[24;21m"
    );

    let binary = config(r"\xFF+", "bold");
    assert_eq!(
        render(&binary, &[0xff, 0xff, b'!']),
        [b"\x1b[1m".as_slice(), &[0xff, 0xff], b"\x1b[22m!"].concat()
    );

    // Non-SGR controls are match barriers, and no generated SGR lands inside
    // OSC, DCS, or cursor-control bytes.
    let broad = config("(?s)a.*b", "bold");
    for input in [
        b"a\x1b]0;window title\x07b".as_slice(),
        b"a\x1b[2Cb".as_slice(),
        b"a\x1bPpayload\x1b\\b".as_slice(),
        b"a\rb".as_slice(),
        b"a\r\nb".as_slice(),
        b"a\x0bb".as_slice(),
        b"a\x0cb".as_slice(),
    ] {
        assert_eq!(render(&broad, input), input);
    }

    // Matching restarts around every screen-record separator. This preserves
    // ^/$ semantics and ensures a rejected broad alternative cannot hide a
    // valid match in the next terminal record.
    let anchored = config("^error$", "fg:red");
    assert_eq!(
        render(&anchored, b"error\rerror\r\nerror\nerror\x0berror\x0c"),
        b"\x1b[31merror\x1b[39m\r\x1b[31merror\x1b[39m\r\n\x1b[31merror\x1b[39m\n\x1b[31merror\x1b[39m\x0b\x1b[31merror\x1b[39m\x0c"
    );
    let alternative = config("(?s)a.*b|b", "fg:red");
    assert_eq!(render(&alternative, b"a\rb"), b"a\r\x1b[31mb\x1b[39m");
    assert_eq!(
        render(&config("^b$", "fg:red"), b"a\x1b[2Cb"),
        b"a\x1b[2C\x1b[31mb\x1b[39m"
    );

    // SGR controls are intentionally transparent to matching.
    let sgr_split = render(&red, b"er\x1b[34mror");
    assert!(contains_bytes(&sgr_split, b"\x1b[34m"));
    assert!(contains_bytes(&sgr_split, b"\x1b[31m"));

    let mut stream = StreamHighlighter::new(config("error", "fg:red"));
    let mut output = Vec::new();
    stream.push(b"er", &mut output).expect("first read");
    assert!(output.is_empty());
    assert_eq!(stream.buffered_len(), 2);
    stream.push(b"ror\n", &mut output).expect("second read");
    assert_eq!(output, b"\x1b[31merror\x1b[39m\n");
    assert_eq!(stream.buffered_len(), 0);

    // CR, vertical whitespace, and non-SGR controls are complete terminal
    // record boundaries too, so progress redraws do not wait for idle flush.
    let mut stream = StreamHighlighter::new(config("^error$", "fg:red"));
    output.clear();
    stream.push(b"error\r", &mut output).expect("CR record");
    assert_eq!(output, b"\x1b[31merror\x1b[39m\r");
    assert_eq!(stream.buffered_len(), 0);
    output.clear();
    stream
        .push(b"error\x1b[2C", &mut output)
        .expect("control-delimited record");
    assert_eq!(output, b"\x1b[31merror\x1b[39m\x1b[2C");
    assert_eq!(stream.buffered_len(), 0);

    // An ANSI sequence can itself be split across reads without corruption.
    let mut stream = StreamHighlighter::new(config("error", "fg:red"));
    output.clear();
    stream.push(b"\x1b[", &mut output).expect("partial CSI");
    assert!(output.is_empty());
    stream
        .push(b"34merror\n", &mut output)
        .expect("completed CSI");
    assert_eq!(output, b"\x1b[34m\x1b[31merror\x1b[34m\n");

    // Idle flush emits a prompt, remains reusable, and safely tracks a control
    // sequence that happened to be split by that explicit commit boundary.
    let mut stream = StreamHighlighter::new(config("error", "underline"));
    output.clear();
    stream.push(b"\x1b[3", &mut output).expect("partial CSI");
    stream.flush(&mut output).expect("idle flush");
    assert_eq!(output, b"\x1b[3");
    stream
        .push(b"1merror\n", &mut output)
        .expect("resume after idle flush");
    assert_eq!(output, b"\x1b[31m\x1b[4merror\x1b[24m\n");
    output.clear();
    stream.push(b"error", &mut output).expect("reused stream");
    assert!(output.is_empty());
    stream.flush(&mut output).expect("second idle flush");
    assert_eq!(output, b"\x1b[4merror\x1b[24m");

    // Internal buffering remains bounded for arbitrarily long records.
    let empty = Config::from_yaml("rules: []\n", ColorDepth::TrueColor).unwrap();
    let mut stream = StreamHighlighter::new(empty);
    let long = vec![b'x'; MAX_PENDING_BYTES * 3 + 17];
    output.clear();
    stream.push(&long, &mut output).expect("long record");
    assert!(stream.buffered_len() <= MAX_PENDING_BYTES);
    stream.flush(&mut output).expect("long record flush");
    assert_eq!(output, long);

    // A pathological unterminated OSC is passed through opaquely once it hits
    // the bound; its later terminator is never mistaken for highlightable text.
    let mut stream = StreamHighlighter::new(config("error", "fg:red"));
    let mut osc = b"\x1b]0;".to_vec();
    osc.extend(std::iter::repeat_n(b'x', MAX_PENDING_BYTES + 31));
    output.clear();
    stream.push(&osc, &mut output).expect("oversized OSC");
    assert_eq!(output, osc);
    assert_eq!(stream.buffered_len(), 0);
    stream
        .push(b"\x07error\n", &mut output)
        .expect("OSC terminator and text");
    assert_eq!(
        &output[..osc.len() + 1],
        &[osc.as_slice(), b"\x07"].concat()
    );
    assert!(contains_bytes(&output[osc.len() + 1..], b"\x1b[31merror"));

    println!("ANSI and stream verification passed");
}
