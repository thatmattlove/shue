use std::fs;
use std::path::Path;

use shue_core::{ColorDepth, Config};

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

fn first_placeholder(document: &str) -> Option<&'static str> {
    ["TODO", "TBD", "example.com", "<repository-url>"]
        .into_iter()
        .find(|placeholder| document.contains(placeholder))
}

#[test]
fn documentation_and_licenses_cover_the_public_contract() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let readme = read(&root.join("README.md"));
    let example_path = root.join("examples/config.yaml");
    let example = read(&example_path);

    for required in [
        "Why Rust",
        "username@host",
        "--ssh-path",
        "SHUE_SSH",
        "PATH",
        "--exec",
        "--filter",
        "--config",
        "SHUE_CONFIG",
        "XDG_CONFIG_HOME",
        "$HOME/.config/shue/config.yaml",
        "/etc/shue/config.yaml",
        "embedded defaults",
        "auto",
        "ansi16",
        "ansi256",
        "truecolor",
        "NO_COLOR",
        "lookahead",
        "lookbehind",
        "backreference",
        "named capture",
        "fails open",
        "runtime limit",
        "shue: warning:",
        "Content errors are non-fatal",
        "empty configuration",
        "f#rrggbb",
        "rgb(r,g,b)",
        "fg:red",
        "terminal theme",
        "macOS",
        "Linux",
        "Limitations",
        "ChromaTerm2",
    ] {
        assert!(readme.contains(required), "README is missing {required:?}");
    }
    assert_eq!(
        readme.matches("```").count() % 2,
        0,
        "unbalanced code fences"
    );
    // Positive control: prove the same absence checker detects a known bad
    // fixture before trusting its result against the real README.
    assert_eq!(first_placeholder("unfinished TODO text"), Some("TODO"));
    assert_eq!(first_placeholder(&readme), None, "README has a placeholder");
    for unsupported in [
        concat!(".chroma", "term.yml"),
        concat!(".chroma", "term.yaml"),
        concat!("/chroma", "term/"),
    ] {
        assert!(
            !readme.contains(unsupported),
            "README advertises unsupported config target {unsupported:?}"
        );
    }
    assert!(
        !readme.contains("stop startup"),
        "README contains the obsolete fatal-config contract"
    );

    Config::from_yaml(&example, ColorDepth::TrueColor)
        .unwrap_or_else(|error| panic!("{} does not compile: {error}", example_path.display()));
    for feature in [
        "palette:",
        "exclusive: true",
        "(?<=",
        "(?=",
        "(?<status>",
        "fg:rgb(",
        "bg:#",
        "f.",
    ] {
        assert!(example.contains(feature), "example is missing {feature:?}");
    }

    let mit = read(&root.join("LICENSE-MIT"));
    assert!(mit.starts_with("MIT License\n\nCopyright (c) 2026 shue contributors"));
    assert!(mit.contains("THE SOFTWARE IS PROVIDED \"AS IS\""));
    assert!(readme.contains("Shue is licensed under the MIT License"));

    println!("documentation verification passed");
}
