#![cfg(unix)]

use std::env;
use std::io::Read;

use crossterm::terminal::is_raw_mode_enabled;
use shue_runtime::{PtySession, RawModeGuard, TerminalSize};

#[test]
fn guard_restores_raw_mode_on_drop_and_error_paths() {
    let current_test_executable = env::current_exe().unwrap();
    let arguments = [
        "--ignored",
        "--exact",
        "raw_mode_helper_inside_a_real_pty",
        "--nocapture",
        "--test-threads=1",
    ]
    .into_iter()
    .map(Into::into)
    .collect::<Vec<_>>();
    let mut session = PtySession::spawn(
        current_test_executable.as_os_str(),
        &arguments,
        TerminalSize::default(),
    )
    .unwrap();
    let mut reader = session.try_clone_reader().unwrap();
    let mut output = Vec::new();
    reader.read_to_end(&mut output).unwrap();
    let exit_code = session.wait().unwrap();

    assert_eq!(
        exit_code,
        0,
        "raw-mode helper failed:\n{}",
        String::from_utf8_lossy(&output)
    );
    assert!(
        output
            .windows(b"raw helper passed".len())
            .any(|part| part == b"raw helper passed"),
        "helper marker missing:\n{}",
        String::from_utf8_lossy(&output)
    );
    println!("terminal guard verification passed");
}

#[test]
#[ignore = "launched under a PTY by the parent verification test"]
fn raw_mode_helper_inside_a_real_pty() {
    assert!(!is_raw_mode_enabled().unwrap());
    assert_eq!(TerminalSize::current().unwrap(), TerminalSize::default());

    {
        let first = RawModeGuard::enable().unwrap();
        assert!(first.is_active());
        assert!(is_raw_mode_enabled().unwrap());

        {
            let second = RawModeGuard::enable().unwrap();
            assert!(second.is_active());
            assert!(is_raw_mode_enabled().unwrap());
        }
        assert!(
            is_raw_mode_enabled().unwrap(),
            "nested guard drop restored too early"
        );
    }
    assert!(!is_raw_mode_enabled().unwrap());

    // If a caller has already enabled raw mode, the guard must preserve that
    // pre-existing state instead of claiming and restoring someone else's
    // transition.
    crossterm::terminal::enable_raw_mode().unwrap();
    {
        let borrowed = RawModeGuard::enable().unwrap();
        assert!(borrowed.is_active());
    }
    assert!(is_raw_mode_enabled().unwrap());
    crossterm::terminal::disable_raw_mode().unwrap();
    assert!(!is_raw_mode_enabled().unwrap());

    let error = ordinary_error_path();
    assert!(error.is_err());
    assert!(
        !is_raw_mode_enabled().unwrap(),
        "raw mode leaked through an ordinary error return"
    );

    let mut explicit = RawModeGuard::enable().unwrap();
    explicit.restore().unwrap();
    explicit.restore().unwrap();
    assert!(!explicit.is_active());
    assert!(!is_raw_mode_enabled().unwrap());

    println!("raw helper passed");
}

fn ordinary_error_path() -> std::io::Result<()> {
    let _guard = RawModeGuard::enable()?;
    assert!(is_raw_mode_enabled()?);
    Err(std::io::Error::other("intentional test error"))
}
