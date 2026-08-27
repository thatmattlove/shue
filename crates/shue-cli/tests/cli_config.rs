#![cfg(unix)]

mod common;

use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::process::Stdio;

use common::{RED_CONFIG, error_text, shue_command, write_config};
use shue::cli::{ColorDepthChoice, Mode, ParseResult, parse};
use shue::config::{ConfigOrigin, ConfigSearch, EMBEDDED_DEFAULT_CONFIG, load_config};
use tempfile::tempdir;

#[test]
fn parsing_discovery_environment_controls_and_help_are_deterministic() {
    let temporary = tempdir().expect("temporary directory");
    let root = temporary.path();
    let home = root.join("home");
    let user = root.join("user-config");
    let system = root.join("system-config");
    fs::create_dir_all(&home).unwrap();

    let yaml = write_config(&user, "shue/config.yaml", "rules: []\n# yaml-first\n");
    let yml = write_config(&user, "shue/config.yml", "rules: []\n# yml-second\n");
    let _foreign_home_yml = write_config(&home, ".chromaterm.yml", "rules: []\n# must-not-load\n");
    let _foreign_home_yaml =
        write_config(&home, ".chromaterm.yaml", "rules: []\n# must-not-load\n");
    let _foreign_user_yml = write_config(
        &user,
        "chromaterm/chromaterm.yml",
        "rules: []\n# must-not-load\n",
    );
    let system_yaml = write_config(&system, "shue/config.yaml", "rules: []\n# system-yaml\n");
    let system_yml = write_config(&system, "shue/config.yml", "rules: []\n# system-yml\n");
    let _foreign_system_yml = write_config(
        &system,
        "chromaterm/chromaterm.yml",
        "rules: []\n# must-not-load\n",
    );
    let search = ConfigSearch {
        user_config_dir: Some(user.clone()),
        system_config_dirs: vec![system.clone()],
    };

    let discovered = load_config(None, None, &search).expect("discover native YAML config");
    assert_eq!(discovered.origin, ConfigOrigin::Discovered(yaml.clone()));
    assert!(discovered.contents.contains("yaml-first"));

    fs::remove_file(&yaml).expect("remove first extension");
    let second = load_config(None, None, &search).expect("discover native YML config");
    assert_eq!(
        second.origin,
        ConfigOrigin::Discovered(user.join("shue/config.yml"))
    );
    assert!(second.contents.contains("yml-second"));

    fs::remove_file(&yml).expect("remove second user extension");
    let selected_system = load_config(None, None, &search).expect("discover system config");
    assert_eq!(
        selected_system.origin,
        ConfigOrigin::Discovered(system_yaml.clone())
    );
    fs::remove_file(&system_yaml).expect("remove first system extension");
    let selected_system = load_config(None, None, &search).expect("discover system YML config");
    assert_eq!(
        selected_system.origin,
        ConfigOrigin::Discovered(system_yml.clone())
    );
    fs::remove_file(&system_yml).expect("remove second system extension");

    let embedded = load_config(None, None, &search).expect("ignore non-Shue config targets");
    assert_eq!(embedded.origin, ConfigOrigin::Embedded);
    assert_eq!(embedded.contents, EMBEDDED_DEFAULT_CONFIG);

    let environment = write_config(root, "environment.yml", "rules: []\n# environment\n");
    let explicit = write_config(root, "explicit.yml", "rules: []\n# explicit\n");
    let selected = load_config(Some(&explicit), Some(environment.as_os_str()), &search)
        .expect("load explicit CLI config");
    assert_eq!(selected.origin, ConfigOrigin::Cli(explicit));
    assert!(selected.contents.contains("explicit"));
    let selected =
        load_config(None, Some(environment.as_os_str()), &search).expect("load environment config");
    assert_eq!(selected.origin, ConfigOrigin::Environment(environment));

    let embedded = load_config(
        None,
        None,
        &ConfigSearch {
            user_config_dir: None,
            system_config_dirs: vec![root.join("absent")],
        },
    )
    .expect("fall back to embedded defaults");
    assert_eq!(embedded.origin, ConfigOrigin::Embedded);
    assert_eq!(embedded.contents, EMBEDDED_DEFAULT_CONFIG);
    for useful_default in ["IPv4", "IPv6", "URLs", "Healthy", "Warning", "Error"] {
        assert!(embedded.contents.contains(useful_default));
    }
    shue_core::Config::from_yaml(&embedded.contents, shue_core::ColorDepth::Ansi16)
        .expect("embedded defaults compile");

    let parsed = parse(
        [
            "--color-depth",
            "ansi256",
            "username@host",
            "remote-command",
            "--config",
            "remote-value",
        ]
        .into_iter()
        .map(OsString::from),
    )
    .expect("parse SSH boundary");
    let ParseResult::Run(parsed) = parsed else {
        panic!("expected run mode")
    };
    assert_eq!(parsed.color_depth, Some(ColorDepthChoice::Ansi256));
    assert_eq!(parsed.config_path, None);
    assert_eq!(
        parsed.mode,
        Mode::Ssh {
            args: [
                "username@host",
                "remote-command",
                "--config",
                "remote-value"
            ]
            .into_iter()
            .map(OsString::from)
            .collect()
        }
    );
    let delimited = parse(
        ["--", "--ssh-path", "remote"]
            .into_iter()
            .map(OsString::from),
    )
    .expect("parse explicit delimiter");
    let ParseResult::Run(delimited) = delimited else {
        panic!("expected run mode")
    };
    assert_eq!(
        delimited.mode,
        Mode::Ssh {
            args: ["--ssh-path", "remote"]
                .into_iter()
                .map(OsString::from)
                .collect()
        }
    );

    let help = shue_command().arg("--help").output().expect("show help");
    assert!(help.status.success());
    let help = String::from_utf8(help.stdout).expect("UTF-8 help");
    for required in [
        "username@host",
        "--ssh-path",
        "SHUE_SSH",
        "--exec",
        "--filter",
        "--color-depth",
    ] {
        assert!(help.contains(required), "missing {required:?} from help");
    }

    let depth_config = write_config(root, "depth.yml", RED_CONFIG);
    let mut depth_child = shue_command();
    depth_child
        .args(["--config"])
        .arg(&depth_config)
        .args(["--color-depth", "truecolor", "--filter"])
        .env("SHUE_COLOR_DEPTH", "ansi16")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
    let mut depth_child = depth_child.spawn().expect("spawn depth precedence filter");
    depth_child
        .stdin
        .take()
        .unwrap()
        .write_all(b"ID:ERROR!\n")
        .unwrap();
    let depth_output = depth_child.wait_with_output().unwrap();
    assert!(
        depth_output.status.success(),
        "{}",
        error_text(&depth_output)
    );
    let truecolor_sgr = b"\x1b[38;2;255;0;0m";
    assert!(
        depth_output
            .stdout
            .windows(truecolor_sgr.len())
            .any(|window| window == truecolor_sgr),
        "unexpected truecolor output: {:?}",
        depth_output.stdout
    );

    let mut no_color_child = shue_command();
    no_color_child
        .args(["--config"])
        .arg(&depth_config)
        .arg("--filter")
        .env("NO_COLOR", "")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
    let mut no_color_child = no_color_child.spawn().expect("spawn NO_COLOR filter");
    no_color_child
        .stdin
        .take()
        .unwrap()
        .write_all(b"ID:ERROR!\n")
        .unwrap();
    let no_color_output = no_color_child.wait_with_output().unwrap();
    assert_eq!(no_color_output.stdout, b"ID:ERROR!\n");

    println!("CLI and config verification passed");
}
