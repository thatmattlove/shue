#![cfg(unix)]

mod common;

use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::process::Stdio;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use common::{
    RED_CONFIG, TestDeadline, error_text, shue_command, strip_sgr, write_config, write_program,
};
use shue_runtime::{PtySession, TerminalSize};
use tempfile::tempdir;

fn filter(config: &std::path::Path, extra: &[&str], input: &[u8]) -> std::process::Output {
    let mut command = shue_command();
    command
        .args(["--config"])
        .arg(config)
        .args(extra)
        .arg("--filter")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().expect("spawn filter");
    child
        .stdin
        .take()
        .expect("filter stdin")
        .write_all(input)
        .expect("write binary filter input");
    child.wait_with_output().expect("wait for filter")
}

fn read_until(
    receiver: &mpsc::Receiver<Vec<u8>>,
    output: &mut Vec<u8>,
    needle: &[u8],
    timeout: Duration,
) -> bool {
    let deadline = Instant::now() + timeout;
    while !output.windows(needle.len()).any(|window| window == needle) {
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return false;
        };
        let Ok(chunk) = receiver.recv_timeout(remaining) else {
            return false;
        };
        output.extend(chunk);
    }
    true
}

fn wait_for_pty_exit(terminal: &mut PtySession) -> u32 {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Some(code) = terminal.try_wait().expect("poll PTY-wrapped shue") {
            return code;
        }
        assert!(Instant::now() < deadline, "PTY-wrapped shue did not exit");
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn filter_and_exec_are_colored_binary_safe_and_recover_cleanly() {
    let _deadline =
        TestDeadline::start("filter_and_exec_are_colored_binary_safe_and_recover_cleanly");
    let temporary = tempdir().expect("temporary directory");
    let config = write_config(temporary.path(), "rules.yml", RED_CONFIG);
    let input = b"\xffID:ERROR!\n";

    let truecolor = filter(&config, &["--color-depth", "truecolor"], input);
    assert!(truecolor.status.success(), "{}", error_text(&truecolor));
    let truecolor_sgr = b"\x1b[38;2;255;0;0m";
    assert!(
        truecolor
            .stdout
            .windows(truecolor_sgr.len())
            .any(|window| window == truecolor_sgr),
        "missing truecolor SGR in {:?}",
        truecolor.stdout
    );
    assert_eq!(strip_sgr(&truecolor.stdout), input);

    let no_color = filter(&config, &["--no-color"], input);
    assert!(no_color.status.success());
    assert_eq!(no_color.stdout, input);

    let program = write_program(
        &temporary.path().join("bin"),
        "emit",
        "exec",
        "printf '\\377ID:ERROR!\\n'\nexit 23",
    );
    let mut wrapped_command = shue_command();
    let wrapped = wrapped_command
        .args(["--config"])
        .arg(&config)
        .args(["--color-depth", "ansi16", "--exec"])
        .arg(&program)
        .arg("--filter")
        .output()
        .expect("run explicit program mode");
    assert_eq!(wrapped.status.code(), Some(23));
    assert!(wrapped.stdout.starts_with(b"PROGRAM:exec\n"));
    assert!(
        wrapped
            .stdout
            .windows(5)
            .any(|window| window == b"\x1b[91m")
    );
    assert_eq!(strip_sgr(&wrapped.stdout), b"PROGRAM:exec\n\xffID:ERROR!\n");

    let mode_error = shue_command()
        .args(["--filter", "unexpected"])
        .output()
        .expect("run invalid filter invocation");
    assert!(!mode_error.status.success());
    let stderr = error_text(&mode_error);
    assert!(stderr.starts_with("shue: "), "{stderr:?}");
    assert!(stderr.contains("--filter does not accept"), "{stderr:?}");

    let partially_invalid_config = write_config(
        temporary.path(),
        "partially-invalid.yml",
        r#"rules:
  - description: invalid rule is ignored
    regex: '('
    color: 'fg:red'
  - description: valid rule is retained
    regex: 'SAFE'
    color: 'fg:green'
"#,
    );
    let recovered = filter(
        &partially_invalid_config,
        &["--color-depth", "ansi16"],
        b"bad SAFE\n",
    );
    assert!(recovered.status.success(), "{}", error_text(&recovered));
    assert_eq!(strip_sgr(&recovered.stdout), b"bad SAFE\n");
    assert!(
        recovered
            .stdout
            .windows(4)
            .any(|window| window == b"\x1b[32"),
        "valid rule was not retained: {:?}",
        recovered.stdout
    );
    let stderr = error_text(&recovered);
    assert!(stderr.starts_with("shue: warning: "), "{stderr:?}");
    assert_eq!(stderr.matches("shue: warning:").count(), 1, "{stderr:?}");
    assert!(stderr.contains("rule 1"), "{stderr:?}");
    assert!(
        stderr.to_ascii_lowercase().contains("regular expression"),
        "{stderr:?}"
    );

    let malformed_config = write_config(temporary.path(), "malformed.yml", "rules: [\n");
    let malformed = filter(&malformed_config, &[], b"unchanged\n");
    assert!(malformed.status.success(), "{}", error_text(&malformed));
    assert_eq!(malformed.stdout, b"unchanged\n");
    let stderr = error_text(&malformed);
    assert!(stderr.starts_with("shue: warning: "), "{stderr:?}");
    assert_eq!(stderr.matches("shue: warning:").count(), 1, "{stderr:?}");
    assert!(stderr.contains("malformed.yml"), "{stderr:?}");
    assert!(stderr.to_ascii_lowercase().contains("yaml"), "{stderr:?}");

    let limited_config = write_config(
        temporary.path(),
        "runtime-limit.yml",
        r#"rules:
  - regex: '(*LIMIT_MATCH=1)^(a+)+$'
    color: 'fg:red'
  - regex: 'safe'
    color: 'fg:green'
"#,
    );
    let hostile =
        b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa! safe\naaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa! safe\n";
    let fail_open = filter(&limited_config, &["--color-depth", "ansi16"], hostile);
    assert!(fail_open.status.success(), "{}", error_text(&fail_open));
    assert_eq!(strip_sgr(&fail_open.stdout), hostile);
    assert!(
        fail_open
            .stdout
            .windows(4)
            .any(|window| window == b"\x1b[32")
    );
    let warning = error_text(&fail_open);
    assert_eq!(
        warning
            .matches("shue: warning: disabling rule 1 after regex runtime error:")
            .count(),
        1,
        "runtime warning must be emitted exactly once: {warning:?}"
    );
    assert!(
        warning.to_ascii_lowercase().contains("match limit"),
        "{warning:?}"
    );

    // Exercise shue with terminal stdin/stdout, rather than relying only on
    // pipe-mode tests. The wrapped program leaves its prompt unterminated and
    // waits for input; coloring must arrive via the idle flush before the test
    // sends the newline that lets the child exit.
    let prompt_program = write_program(
        &temporary.path().join("bin"),
        "prompt",
        "prompt",
        "printf 'ID:ERROR!'\nIFS= read -r ignored",
    );
    let arguments = vec![
        OsString::from("-u"),
        OsString::from("NO_COLOR"),
        OsString::from(env!("CARGO_BIN_EXE_shue")),
        OsString::from("--config"),
        config.as_os_str().to_owned(),
        OsString::from("--color-depth"),
        OsString::from("truecolor"),
        OsString::from("--exec"),
        prompt_program.as_os_str().to_owned(),
    ];
    let mut terminal = PtySession::spawn(
        std::ffi::OsStr::new("/usr/bin/env"),
        &arguments,
        TerminalSize::default(),
    )
    .expect("spawn shue in an outer PTY");
    let mut reader = terminal.try_clone_reader().expect("clone outer PTY reader");
    let mut writer = terminal.take_writer().expect("take outer PTY writer");
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut buffer = [0_u8; 4096];
        loop {
            match std::io::Read::read(&mut reader, &mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    if sender.send(buffer[..read].to_vec()).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
    let mut prompt_output = Vec::new();
    // Process startup can be slow on a busy runner. Start the display-latency
    // deadline once the child's startup banner reaches the terminal.
    assert!(
        read_until(
            &receiver,
            &mut prompt_output,
            b"PROGRAM:prompt",
            Duration::from_secs(5),
        ),
        "interactive child did not start: {prompt_output:?}"
    );
    assert!(
        read_until(
            &receiver,
            &mut prompt_output,
            truecolor_sgr,
            Duration::from_millis(700),
        ),
        "interactive prompt was not idle-flushed: {prompt_output:?}"
    );
    writer
        .write_all(b"\n")
        .expect("send input after observing prompt");
    writer.flush().expect("flush outer PTY input");
    assert_eq!(wait_for_pty_exit(&mut terminal), 0);

    // CR-delimited progress updates can arrive continuously, so the idle path
    // never runs. They still need a wall-clock writer flush; this producer
    // continues until the test proves an update was visible and creates the
    // stop file.
    let progress_stop = temporary.path().join("progress-stop");
    let progress_done = temporary.path().join("progress-done");
    let progress_program = write_program(
        &temporary.path().join("bin"),
        "progress",
        "continuous-control",
        r#"stop_file=$1
done_file=$2
/bin/sleep 0.05
i=0
while [ ! -f "$stop_file" ]; do
  printf '\rprogress-%s' "$i"
  i=$((i + 1))
  /bin/sleep 0.005
done
printf '%s\n' done > "$done_file""#,
    );
    let progress_arguments = vec![
        OsString::from("-u"),
        OsString::from("NO_COLOR"),
        OsString::from(env!("CARGO_BIN_EXE_shue")),
        OsString::from("--config"),
        config.as_os_str().to_owned(),
        OsString::from("--color-depth"),
        OsString::from("truecolor"),
        OsString::from("--exec"),
        progress_program.as_os_str().to_owned(),
        progress_stop.as_os_str().to_owned(),
        progress_done.as_os_str().to_owned(),
    ];
    let mut progress_terminal = PtySession::spawn(
        std::ffi::OsStr::new("/usr/bin/env"),
        &progress_arguments,
        TerminalSize::default(),
    )
    .expect("spawn continuous progress PTY");
    let mut progress_reader = progress_terminal
        .try_clone_reader()
        .expect("clone progress PTY reader");
    let (progress_sender, progress_receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut buffer = [0_u8; 4096];
        loop {
            match std::io::Read::read(&mut progress_reader, &mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    if progress_sender.send(buffer[..read].to_vec()).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
    let mut progress_output = Vec::new();
    assert!(
        read_until(
            &progress_receiver,
            &mut progress_output,
            b"PROGRAM:continuous-control",
            Duration::from_secs(5),
        ),
        "progress child did not start: {progress_output:?}"
    );
    let progress_visible = read_until(
        &progress_receiver,
        &mut progress_output,
        b"progress-",
        Duration::from_secs(1),
    );
    if !progress_visible || progress_done.exists() {
        let _ = progress_terminal.terminate();
        let _ = wait_for_pty_exit(&mut progress_terminal);
        panic!(
            "continuous CR output was not flushed on time: visible={progress_visible}, done={}, output={progress_output:?}",
            progress_done.exists()
        );
    }
    fs::write(&progress_stop, b"stop\n").expect("stop progress producer");
    assert_eq!(wait_for_pty_exit(&mut progress_terminal), 0);
    assert_eq!(
        fs::read_to_string(&progress_done).expect("progress completion marker"),
        "done\n"
    );

    println!("shue mode verification passed");
}
