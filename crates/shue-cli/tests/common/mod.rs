#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

// A panic cannot rescue a test stuck inside a blocking child wait or
// destructor. Declare this guard before the test's other owned resources.
pub struct TestDeadline(mpsc::Sender<()>);

impl TestDeadline {
    pub fn start(name: &'static str) -> Self {
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            if matches!(
                receiver.recv_timeout(Duration::from_secs(30)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ) {
                eprintln!("{name} exceeded its 30-second deadline; terminating the test process");
                std::process::exit(1);
            }
        });
        Self(sender)
    }
}

impl Drop for TestDeadline {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}

pub fn shue_command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_shue"));
    command
        .stdin(Stdio::null())
        .env_remove("NO_COLOR")
        .env_remove("SHUE_COLOR_DEPTH")
        .env_remove("SHUE_CONFIG")
        .env_remove("SHUE_SSH");
    command
}

pub fn write_config(directory: &Path, name: &str, contents: &str) -> PathBuf {
    let path = directory.join(name);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create config parent");
    }
    fs::write(&path, contents).expect("write config");
    path
}

#[cfg(unix)]
pub fn write_program(directory: &Path, name: &str, label: &str, body: &str) -> PathBuf {
    fs::create_dir_all(directory).expect("create program directory");
    let path = directory.join(name);
    let source = format!("#!/bin/sh\nprintf '%s\\n' 'PROGRAM:{label}'\n{body}\n",);
    fs::write(&path, source).expect("write fake program");
    let mut permissions = fs::metadata(&path)
        .expect("fake program metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&path, permissions).expect("make fake program executable");
    path
}

pub fn output_text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

pub fn error_text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

pub fn strip_sgr(input: &[u8]) -> Vec<u8> {
    let mut result = Vec::with_capacity(input.len());
    let mut cursor = 0;
    while cursor < input.len() {
        if input[cursor] == 0x1b && input.get(cursor + 1) == Some(&b'[') {
            let mut end = cursor + 2;
            while end < input.len() && (input[end].is_ascii_digit() || input[end] == b';') {
                end += 1;
            }
            if input.get(end) == Some(&b'm') {
                cursor = end + 1;
                continue;
            }
        }
        result.push(input[cursor]);
        cursor += 1;
    }
    result
}

pub const EMPTY_CONFIG: &str = "palette: {}\nrules: []\n";

pub const RED_CONFIG: &str = r#"palette: {}
rules:
  - description: Lookbehind and lookahead acceptance
    regex: '(?<=ID:)ERROR(?=!)'
    color: 'fg:#ff0000'
"#;
