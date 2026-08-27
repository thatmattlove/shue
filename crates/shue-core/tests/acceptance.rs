use shue_core::{ColorDepth, Config, StreamHighlighter};

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

#[test]
fn root_core_acceptance_contract() {
    let yaml = r##"
rules:
  - regex: '(?<=ID:)ERROR(?=!)'
    color: 'fg:red bold'
  - regex: '(?<octet>\d{1,3})(?=ms)'
    color:
      octet: 'bg:rgb(1, 2, 3) underline'
  - regex: '\xFF+'
    color: 'fg:#123456'
"##;
    let config = Config::from_yaml(yaml, ColorDepth::TrueColor).expect("acceptance config");
    let input = b"\x1b[34mID:ERROR! 42ms \xff\x1b[0m";
    let mut output = Vec::new();
    config.highlight(input, &mut output).expect("highlight");

    assert!(contains_bytes(&output, b"\x1b[1;31mERROR"));
    assert!(contains_bytes(&output, b"\x1b[4;48;2;1;2;3m42"));
    assert!(contains_bytes(&output, b"\x1b[38;2;18;52;86m\xff"));
    assert!(contains_bytes(&output, b"\x1b[34m"));
    assert!(output.ends_with(b"\x1b[0m"));

    let native = Config::from_yaml(
        "rules:\n  - regex: themed\n    color: 'fg:green bg:bright-black'\n",
        ColorDepth::TrueColor,
    )
    .unwrap();
    let mut native_output = Vec::new();
    native.highlight(b"themed", &mut native_output).unwrap();
    assert_eq!(
        native_output, b"\x1b[32;100mthemed\x1b[39;49m",
        "native names must use terminal theme slots, not hard-coded RGB"
    );

    let streaming = Config::from_yaml(
        "rules:\n  - regex: prompt\n    color: 'fg:yellow'\n",
        ColorDepth::Ansi16,
    )
    .unwrap();
    let mut stream = StreamHighlighter::new(streaming);
    let mut streamed = Vec::new();
    stream.push(b"pro", &mut streamed).unwrap();
    assert!(streamed.is_empty());
    stream.push(b"mpt", &mut streamed).unwrap();
    assert!(streamed.is_empty());
    stream.flush(&mut streamed).unwrap();
    assert_eq!(streamed, b"\x1b[33mprompt\x1b[39m");

    println!("shue-core acceptance verification passed");
}
