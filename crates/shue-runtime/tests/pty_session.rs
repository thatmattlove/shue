#![cfg(unix)]

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::PermissionsExt;

use shue_runtime::{PtySession, TerminalSize};
use tempfile::TempDir;

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

#[test]
fn real_pty_supports_arguments_io_resize_and_exit_status() {
    let temporary = TempDir::new().unwrap();
    let program = temporary.path().join("pty-helper");
    fs::write(
        &program,
        b"#!/bin/sh\nprintf 'first:%s\\n' \"$1\"\nprintf 'second:%s\\n' \"$2\"\nprintf 'ready\\n'\nIFS= read -r line\nprintf 'reply:%s\\n' \"$line\"\nstty size\nexit \"$3\"\n",
    )
    .unwrap();
    fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();

    let first = OsString::from_vec(b"native-\xff-argument".to_vec());
    let second = OsString::from("space ; $() ' argument");
    let arguments = vec![first, second, OsString::from("23")];
    let initial_size = TerminalSize {
        rows: 24,
        cols: 80,
        pixel_width: 0,
        pixel_height: 0,
    };
    let mut session = PtySession::spawn(program.as_os_str(), &arguments, initial_size).unwrap();
    let mut reader = session.try_clone_reader().unwrap();
    let mut writer = session.take_writer().unwrap();
    assert!(
        session.take_writer().is_err(),
        "PTY unexpectedly handed out a second input stream"
    );

    let resized = TerminalSize {
        rows: 37,
        cols: 113,
        pixel_width: 1280,
        pixel_height: 720,
    };
    session.resize(resized).unwrap();
    writer.write_all(b"from-parent\n").unwrap();
    writer.flush().unwrap();
    drop(writer);

    let mut output = Vec::new();
    reader.read_to_end(&mut output).unwrap();
    let exit_code = session.wait().unwrap();

    assert_eq!(exit_code, 23, "nonzero child status must propagate");
    assert!(contains(&output, b"first:native-\xff-argument"));
    assert!(contains(&output, b"second:space ; $() ' argument"));
    assert!(contains(&output, b"reply:from-parent"));
    assert!(
        contains(&output, b"37 113"),
        "child must observe the resized PTY; output was {:?}",
        String::from_utf8_lossy(&output)
    );

    // A missing executable must fail directly rather than being interpreted by
    // a command shell.
    let missing = temporary.path().join("does-not-exist");
    assert!(
        PtySession::spawn(OsStr::new(&missing), &[], initial_size).is_err(),
        "spawn unexpectedly accepted a missing program"
    );

    let long_running = temporary.path().join("long-running-helper");
    fs::write(
        &long_running,
        b"#!/bin/sh\nprintf 'long-running-ready\\n'\nwhile :; do sleep 1; done\n",
    )
    .unwrap();
    fs::set_permissions(&long_running, fs::Permissions::from_mode(0o755)).unwrap();
    let mut terminated = PtySession::spawn(long_running.as_os_str(), &[], initial_size).unwrap();
    let mut terminated_output = BufReader::new(terminated.try_clone_reader().unwrap());
    let mut ready = String::new();
    terminated_output.read_line(&mut ready).unwrap();
    assert!(ready.contains("long-running-ready"));
    terminated.terminate().unwrap();
    let terminated_code = terminated.wait().unwrap();
    assert_eq!(
        terminated_code, 129,
        "portable-pty SIGHUP must use shell-compatible 128+signal status"
    );

    println!("PTY session verification passed");
}
