#![cfg(unix)]

mod common;

use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::{Read, Write};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use common::{
    EMPTY_CONFIG, TestDeadline, error_text, output_text, shue_command, write_config, write_program,
};
use nix::errno::Errno;
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use shue_runtime::{PtySession, TerminalSize};
use tempfile::tempdir;

const LIFECYCLE_PID_FILE: &str = "SHUE_TEST_LIFECYCLE_PID_FILE";
const LIFECYCLE_TERMINATED_MARKER: &str = "SHUE_TEST_LIFECYCLE_TERMINATED_MARKER";

#[test]
fn lifecycle_signal_helper() {
    let Some(pid_file) = env::var_os(LIFECYCLE_PID_FILE) else {
        return;
    };
    let marker_file =
        env::var_os(LIFECYCLE_TERMINATED_MARKER).expect("lifecycle helper termination marker path");
    let terminated = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGTERM, Arc::clone(&terminated))
        .expect("register lifecycle helper termination flag");
    let mut worker = Command::new("/bin/sleep")
        .arg("30")
        .spawn()
        .expect("spawn lifecycle helper descendant");
    fs::write(
        pid_file,
        format!("{} {}\n", std::process::id(), worker.id()),
    )
    .expect("publish lifecycle helper PIDs");

    while !terminated.load(Ordering::Relaxed) {
        thread::sleep(Duration::from_millis(5));
    }
    fs::write(marker_file, "terminated\n").expect("write lifecycle termination marker");
    let _ = worker.wait();
}

fn wait_for_child_pids(path: &std::path::Path) -> (i32, i32) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if let Ok(contents) = fs::read_to_string(path) {
            let pids: Vec<_> = contents
                .split_whitespace()
                .filter_map(|value| value.parse::<i32>().ok())
                .collect();
            if let [child, descendant] = pids.as_slice() {
                return (*child, *descendant);
            }
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!(
        "timed out waiting for child PID fixture at {}",
        path.display()
    );
}

fn assert_process_gone(pid: i32) {
    match kill(Pid::from_raw(pid), None) {
        Err(Errno::ESRCH) => {}
        Ok(()) => panic!("process {pid} is still alive"),
        Err(error) => panic!("unable to check process {pid}: {error}"),
    }
}

fn wait_with_output_deadline(mut child: Child, cleanup_pids: &[i32], description: &str) -> Output {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return child.wait_with_output().expect("collect wrapper output"),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(None) => break,
            Err(error) => panic!("unable to poll {description}: {error}"),
        }
    }

    // A regression in the cleanup path must fail promptly rather than consume
    // the outer test timeout or leave its fixture processes behind.
    for pid in cleanup_pids {
        let _ = kill(Pid::from_raw(*pid), Signal::SIGKILL);
    }
    let _ = child.kill();
    let output = child
        .wait_with_output()
        .expect("collect timed-out wrapper output");
    panic!(
        "{description} did not exit within three seconds; status={:?}, stderr={}",
        output.status,
        error_text(&output)
    );
}

#[test]
fn direct_ssh_precedence_passthrough_and_exit_codes() {
    let _deadline = TestDeadline::start("direct_ssh_precedence_passthrough_and_exit_codes");
    let temporary = tempdir().expect("temporary directory");
    let root = temporary.path();
    let config = write_config(root, "empty.yml", EMPTY_CONFIG);

    let cli_ssh = write_program(
        &root.join("cli"),
        "ssh-cli",
        "cli",
        r#"for argument in "$@"; do
  printf 'ARG:<%s>\n' "$argument"
done
exit "${SHUE_FAKE_EXIT:-0}""#,
    );
    let env_ssh = write_program(
        &root.join("env"),
        "ssh-env",
        "env",
        "exit \"${SHUE_FAKE_EXIT:-0}\"",
    );
    let path_dir = root.join("path");
    let _path_ssh = write_program(&path_dir, "ssh", "path", "exit \"${SHUE_FAKE_EXIT:-0}\"");

    let cli_output = shue_command()
        .args(["--no-color", "--config"])
        .arg(&config)
        .args(["--ssh-path"])
        .arg(&cli_ssh)
        .args([
            "-p",
            "2222",
            "username@host",
            "remote-command",
            "--config",
            "remote-file",
        ])
        .env("SHUE_SSH", &env_ssh)
        .env("PATH", &path_dir)
        .stdout(Stdio::piped())
        .output()
        .expect("run shue with CLI SSH override");
    assert!(cli_output.status.success(), "{:?}", cli_output.status);
    let cli_text = output_text(&cli_output);
    assert!(cli_text.contains("PROGRAM:cli"), "{cli_text:?}");
    for expected in [
        "ARG:<-p>",
        "ARG:<2222>",
        "ARG:<username@host>",
        "ARG:<remote-command>",
        "ARG:<--config>",
        "ARG:<remote-file>",
    ] {
        assert!(
            cli_text.contains(expected),
            "missing {expected:?}: {cli_text:?}"
        );
    }

    let env_output = shue_command()
        .args(["--no-color", "--config"])
        .arg(&config)
        .arg("username@host")
        .env("SHUE_SSH", &env_ssh)
        .env("PATH", &path_dir)
        .output()
        .expect("run shue with SHUE_SSH override");
    assert!(env_output.status.success());
    assert!(output_text(&env_output).contains("PROGRAM:env"));

    let path_output = shue_command()
        .args(["--no-color", "--config"])
        .arg(&config)
        .arg("username@host")
        .env_remove("SHUE_SSH")
        .env("PATH", &path_dir)
        .output()
        .expect("run shue with PATH discovery");
    assert!(path_output.status.success());
    assert!(output_text(&path_output).contains("PROGRAM:path"));

    let exit_output = shue_command()
        .args(["--no-color", "--config"])
        .arg(&config)
        .arg("username@host")
        .env("SHUE_SSH", &env_ssh)
        .env("SHUE_FAKE_EXIT", "37")
        .output()
        .expect("run exiting fake SSH");
    assert_eq!(exit_output.status.code(), Some(37));

    let empty_path = root.join("empty-path");
    fs::create_dir(&empty_path).expect("create empty PATH");
    let missing_output = shue_command()
        .args(["--no-color", "--config"])
        .arg(&config)
        .arg("username@host")
        .env_remove("SHUE_SSH")
        .env("PATH", &empty_path)
        .output()
        .expect("run with missing SSH");
    assert!(!missing_output.status.success());
    let missing_error = error_text(&missing_output);
    assert!(missing_error.starts_with("shue: "), "{missing_error:?}");
    assert!(missing_error.contains("SSH"), "{missing_error:?}");

    let child_pid_file = root.join("child-pids");
    let terminated_marker = root.join("child-terminated");
    let lifecycle_program = env::current_exe().expect("current e2e test executable");
    let mut lifecycle_command = shue_command();
    lifecycle_command
        .args(["--no-color", "--config"])
        .arg(&config)
        .arg("--exec")
        .arg(&lifecycle_program)
        .args(["--exact", "lifecycle_signal_helper", "--nocapture"])
        .env(LIFECYCLE_PID_FILE, &child_pid_file)
        .env(LIFECYCLE_TERMINATED_MARKER, &terminated_marker)
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let lifecycle_wrapper = lifecycle_command.spawn().expect("spawn lifecycle wrapper");
    let wrapper_pid = Pid::from_raw(lifecycle_wrapper.id() as i32);
    let (child_pid, descendant_pid) = wait_for_child_pids(&child_pid_file);
    kill(wrapper_pid, Signal::SIGTERM).expect("terminate shue wrapper");
    let lifecycle_output = wait_with_output_deadline(
        lifecycle_wrapper,
        &[child_pid, descendant_pid],
        "terminated shue wrapper",
    );
    assert_eq!(
        lifecycle_output.status.code(),
        Some(143),
        "unexpected wrapper status; stderr={}",
        error_text(&lifecycle_output)
    );
    let termination_marker = fs::read_to_string(&terminated_marker).unwrap_or_else(|error| {
        panic!(
            "child termination marker: {error}; stderr={}",
            error_text(&lifecycle_output)
        )
    });
    assert_eq!(termination_marker, "terminated\n");
    assert_process_gone(child_pid);
    assert_process_gone(descendant_pid);

    // Closing stdout must not move shue into an uninterruptible blocking wait.
    // The wrapper continues polling both the child and its signal flag.
    let closed_stdout_pids = root.join("closed-stdout-pids");
    let closed_stdout_program = write_program(
        &root.join("closed-stdout"),
        "closed-stdout-child",
        "closed-stdout",
        r#"pid_file=$1
exec 1>&-
/bin/sleep 30 &
worker=$!
printf '%s %s\n' "$$" "$worker" > "$pid_file"
wait "$worker""#,
    );
    let mut closed_stdout_command = shue_command();
    closed_stdout_command
        .args(["--no-color", "--config"])
        .arg(&config)
        .arg("--exec")
        .arg(&closed_stdout_program)
        .arg(&closed_stdout_pids)
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let closed_stdout_wrapper = closed_stdout_command
        .spawn()
        .expect("spawn closed-stdout wrapper");
    let closed_wrapper_pid = Pid::from_raw(closed_stdout_wrapper.id() as i32);
    let (closed_child_pid, closed_descendant_pid) = wait_for_child_pids(&closed_stdout_pids);
    kill(closed_wrapper_pid, Signal::SIGTERM).expect("terminate closed-stdout wrapper");
    let closed_stdout_output = wait_with_output_deadline(
        closed_stdout_wrapper,
        &[closed_child_pid, closed_descendant_pid],
        "closed-stdout shue wrapper",
    );
    assert_eq!(
        closed_stdout_output.status.code(),
        Some(143),
        "closed-stdout wrapper failed; stderr={}",
        error_text(&closed_stdout_output)
    );
    assert_process_gone(closed_child_pid);
    assert_process_gone(closed_descendant_pid);

    println!("shue end-to-end verification passed");
}

#[test]
fn mixed_terminal_input_and_redirected_stdout_exit_cleanly() {
    let _deadline = TestDeadline::start("mixed_terminal_input_and_redirected_stdout_exit_cleanly");
    let temporary = tempdir().expect("temporary directory");
    let root = temporary.path();
    let config = write_config(root, "empty.yml", EMPTY_CONFIG);

    // Mixed descriptors are common for `shue host | tee`: stdin remains the
    // foreground TTY while stdout is redirected. The wrapped child must stay
    // in that foreground process group so its terminal read is not stopped by
    // SIGTTIN.
    let tty_read_marker = root.join("tty-read-complete");
    let redirected_output = root.join("tty-redirected-output");
    let tty_reader = write_program(
        &root.join("tty-reader"),
        "tty-reader-child",
        "tty-reader",
        r#"marker=$1
IFS= read -r input
printf '%s\n' "$input" > "$marker""#,
    );
    let tty_harness = write_program(
        &root.join("tty-harness"),
        "tty-harness",
        "tty-harness",
        // More than a PTY buffer of startup output makes missing output
        // draining fail reliably, including on platforms without exit drain.
        r#"i=0
while [ "$i" -lt 8192 ]; do
  printf 'terminal startup output\n'
  i=$((i + 1))
done
exec "$1" --no-color --config "$2" --exec "$3" "$4" > "$5""#,
    );
    let tty_arguments = vec![
        OsString::from(env!("CARGO_BIN_EXE_shue")),
        config.as_os_str().to_owned(),
        tty_reader.as_os_str().to_owned(),
        tty_read_marker.as_os_str().to_owned(),
        redirected_output.as_os_str().to_owned(),
    ];
    let mut outer_terminal = PtySession::spawn(
        tty_harness.as_os_str(),
        &tty_arguments,
        TerminalSize::default(),
    )
    .expect("spawn mixed-descriptor PTY harness");
    // macOS drains the controlling terminal when a session leader exits.
    // Even echoed input and the harness banner can otherwise deadlock wait().
    let mut reader = outer_terminal
        .try_clone_reader()
        .expect("clone mixed-descriptor PTY reader");
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut output = Vec::new();
        let result = reader.read_to_end(&mut output).map(|_| output);
        let _ = sender.send(result);
    });
    let mut terminal_writer = outer_terminal
        .take_writer()
        .expect("take mixed-descriptor PTY writer");
    terminal_writer
        .write_all(b"foreground-input\n")
        .expect("write foreground terminal input");
    terminal_writer.flush().expect("flush terminal input");
    let read_deadline = Instant::now() + Duration::from_secs(3);
    while !tty_read_marker.exists() && Instant::now() < read_deadline {
        thread::sleep(Duration::from_millis(10));
    }
    if !tty_read_marker.exists() {
        let _ = outer_terminal.terminate();
        let _ = outer_terminal.wait();
        panic!("mixed-descriptor child did not read foreground TTY input");
    }
    assert_eq!(
        fs::read_to_string(&tty_read_marker).expect("TTY read marker"),
        "foreground-input\n"
    );
    let exit_deadline = Instant::now() + Duration::from_secs(3);
    let exit_code = loop {
        if let Some(code) = outer_terminal
            .try_wait()
            .expect("poll mixed-descriptor harness")
        {
            break code;
        }
        assert!(
            Instant::now() < exit_deadline,
            "mixed-descriptor harness did not exit after reading terminal input"
        );
        thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(exit_code, 0);
    let terminal_output = receiver
        .recv_timeout(Duration::from_secs(3))
        .expect("mixed-descriptor PTY did not reach EOF")
        .expect("read mixed-descriptor terminal output");
    assert!(terminal_output.len() > 128 * 1024);
    assert!(
        String::from_utf8_lossy(&terminal_output).contains("PROGRAM:tty-harness"),
        "harness output did not reach the PTY reader"
    );
    assert!(
        fs::read_to_string(&redirected_output)
            .expect("redirected mixed-descriptor output")
            .contains("PROGRAM:tty-reader"),
        "wrapped program output did not reach redirected stdout"
    );

    println!("mixed-descriptor PTY verification passed");
}
