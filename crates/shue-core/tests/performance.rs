use std::time::Instant;

use shue_core::{ColorDepth, Config, MAX_PENDING_BYTES, StreamHighlighter};

const TARGET_BYTES: usize = 4 * 1024 * 1024;
const RELEASE_MIN_MIB_PER_SECOND: f64 = 5.0;

fn strip_csi(input: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(input.len());
    let mut cursor = 0;
    while cursor < input.len() {
        if input[cursor..].starts_with(b"\x1b[") {
            if let Some(final_offset) = input[cursor + 2..]
                .iter()
                .position(|byte| (0x40..=0x7e).contains(byte))
            {
                cursor += 2 + final_offset + 1;
                continue;
            }
        }
        output.push(input[cursor]);
        cursor += 1;
    }
    output
}

#[test]
fn representative_release_throughput_and_buffer_bound() {
    let yaml = r##"
palette:
  accent: '#5f87ff'
rules:
  - regex: '(?i)\b(?:error|failed|fatal)\b'
    color: 'fg:red bold'
    exclusive: true
  - regex: '(?<=latency=)\d+(?=ms\b)'
    color: 'fg:yellow underline'
  - regex: '\b(?<status>UP|DOWN|DEGRADED)\b'
    color:
      status: 'fg:green bold'
  - regex: '\b(?:\d{1,3}\.){3}\d{1,3}\b'
    color: 'f.accent'
"##;
    let config = Config::from_yaml(yaml, ColorDepth::TrueColor).expect("benchmark config");
    let line = b"Aug 27 host latency=123ms ERROR from 192.0.2.1 status=DOWN\n";
    let repetitions = TARGET_BYTES.div_ceil(line.len());
    let mut input = Vec::with_capacity(repetitions * line.len());
    for _ in 0..repetitions {
        input.extend_from_slice(line);
    }

    let mut output = Vec::with_capacity(input.len() * 2);
    let started = Instant::now();
    config
        .highlight(&input, &mut output)
        .expect("representative highlighting");
    let elapsed = started.elapsed();
    let mib = input.len() as f64 / (1024.0 * 1024.0);
    let throughput = mib / elapsed.as_secs_f64().max(f64::MIN_POSITIVE);
    // The root gate executes this test in release mode. Keep the debug variant
    // useful to `cargo test --all-targets` without pretending unoptimized event
    // sorting is representative of the shipped binary.
    let active_floor = if cfg!(debug_assertions) {
        1.0
    } else {
        RELEASE_MIN_MIB_PER_SECOND
    };

    assert!(output.len() > input.len());
    assert_eq!(strip_csi(&output), input);
    assert!(
        throughput >= active_floor,
        "throughput {throughput:.2} MiB/s was below {active_floor:.2} MiB/s ({} bytes in {elapsed:?})",
        input.len()
    );

    let empty = Config::from_yaml("rules: []\n", ColorDepth::TrueColor).unwrap();
    let mut stream = StreamHighlighter::new(empty);
    let no_newline = vec![b'x'; MAX_PENDING_BYTES * 3];
    let mut streamed = Vec::new();
    stream.push(&no_newline, &mut streamed).unwrap();
    assert!(stream.buffered_len() <= MAX_PENDING_BYTES);
    stream.flush(&mut streamed).unwrap();
    assert_eq!(streamed, no_newline);

    println!(
        "measured {throughput:.2} MiB/s across {} input bytes; release floor {RELEASE_MIN_MIB_PER_SECOND:.2} MiB/s",
        input.len()
    );
    println!("shue-core performance verification passed");
}
