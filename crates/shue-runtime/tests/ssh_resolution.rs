use std::ffi::OsStr;
use std::fs;
use std::io::Read;
use std::path::Path;
use std::process::Command;

use shue_runtime::{
    PtySession, ResolveError, SshPathOptions, TerminalSize, resolve_ssh_path,
    resolve_ssh_path_from_env,
};
use tempfile::TempDir;

#[cfg(unix)]
fn set_executable(path: &Path, executable: bool) {
    use std::os::unix::fs::PermissionsExt;

    let mode = if executable { 0o755 } else { 0o644 };
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

#[cfg(not(unix))]
fn set_executable(_path: &Path, _executable: bool) {}

fn create_candidate(directory: &Path, name: &OsStr, executable: bool) -> std::path::PathBuf {
    let path = directory.join(name);
    fs::write(&path, b"#!/bin/sh\nexit 0\n").unwrap();
    set_executable(&path, executable);
    path
}

fn create_output_candidate(directory: &Path, name: &OsStr, marker: &str) -> std::path::PathBuf {
    let path = directory.join(name);
    fs::write(
        &path,
        format!("#!/bin/sh\nprintf '%s\\n' '{marker}'\n").as_bytes(),
    )
    .unwrap();
    set_executable(&path, true);
    path
}

#[test]
fn precedence_validation_and_os_strings_are_deterministic() {
    let root = TempDir::new().unwrap();
    let cli_dir = root.path().join("cli");
    let env_dir = root.path().join("env");
    let path_dir = root.path().join("path");
    fs::create_dir_all(&cli_dir).unwrap();
    fs::create_dir_all(&env_dir).unwrap();
    fs::create_dir_all(&path_dir).unwrap();

    let cli = create_candidate(&cli_dir, OsStr::new("custom-ssh"), true);
    let env = create_candidate(&env_dir, OsStr::new("custom-ssh"), true);
    let path_candidate = create_candidate(&path_dir, OsStr::new("ssh"), true);
    let rejected_path_dir = root.path().join("rejected-path-entry");
    fs::create_dir(&rejected_path_dir).unwrap();
    create_candidate(&rejected_path_dir, OsStr::new("ssh"), false);
    let path_value = std::env::join_paths([&rejected_path_dir, &path_dir]).unwrap();

    let resolved = resolve_ssh_path(SshPathOptions {
        cli: Some(cli.as_os_str()),
        env: Some(env.as_os_str()),
        path: Some(path_value.as_os_str()),
    })
    .unwrap();
    assert_eq!(resolved, cli, "CLI must have highest priority");

    let resolved = resolve_ssh_path(SshPathOptions {
        cli: None,
        env: Some(env.as_os_str()),
        path: Some(path_value.as_os_str()),
    })
    .unwrap();
    assert_eq!(resolved, env, "SHUE_SSH must outrank PATH");

    let resolved = resolve_ssh_path(SshPathOptions {
        cli: None,
        env: None,
        path: Some(path_value.as_os_str()),
    })
    .unwrap();
    assert_eq!(resolved, path_candidate);

    let missing = root.path().join("missing-ssh");
    assert!(matches!(
        resolve_ssh_path(SshPathOptions {
            cli: Some(missing.as_os_str()),
            env: Some(env.as_os_str()),
            path: Some(path_value.as_os_str()),
        }),
        Err(ResolveError::OverrideNotFound { .. })
    ));

    let non_executable = create_candidate(root.path(), OsStr::new("non-executable"), false);
    assert!(matches!(
        resolve_ssh_path(SshPathOptions {
            cli: None,
            env: Some(non_executable.as_os_str()),
            path: Some(path_value.as_os_str()),
        }),
        Err(ResolveError::OverrideNotExecutable { .. })
    ));

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        // A raw mode-bit check always accepts this because "other execute" is
        // set. Effective access depends on the caller's identity (notably,
        // root differs), so compare against a real launch attempt.
        let wrong_class = create_candidate(root.path(), OsStr::new("wrong-class"), false);
        fs::set_permissions(&wrong_class, fs::Permissions::from_mode(0o001)).unwrap();
        let operating_system_can_execute = Command::new(&wrong_class).status().is_ok();
        let resolver_can_execute = resolve_ssh_path(SshPathOptions {
            cli: Some(wrong_class.as_os_str()),
            env: None,
            path: None,
        })
        .is_ok();
        assert_eq!(
            resolver_can_execute, operating_system_can_execute,
            "resolver executable check disagrees with a real launch attempt"
        );
    }

    assert!(matches!(
        resolve_ssh_path(SshPathOptions {
            cli: Some(OsStr::new("")),
            env: Some(env.as_os_str()),
            path: Some(path_value.as_os_str()),
        }),
        Err(ResolveError::EmptyOverride { .. })
    ));
    assert!(matches!(
        resolve_ssh_path(SshPathOptions::default()),
        Err(ResolveError::PathNotSet)
    ));

    let directory_candidate = root.path().join("not-a-program");
    fs::create_dir(&directory_candidate).unwrap();
    assert!(matches!(
        resolve_ssh_path(SshPathOptions {
            cli: Some(directory_candidate.as_os_str()),
            env: None,
            path: None,
        }),
        Err(ResolveError::OverrideNotFile { .. })
    ));

    #[cfg(target_os = "linux")]
    {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let non_utf8_name = OsString::from_vec(b"ssh-\xff".to_vec());
        let non_utf8 = create_candidate(root.path(), &non_utf8_name, true);
        let resolved = resolve_ssh_path(SshPathOptions {
            cli: Some(non_utf8.as_os_str()),
            env: None,
            path: None,
        })
        .unwrap();
        assert_eq!(resolved, non_utf8);
    }

    #[cfg(all(unix, not(target_os = "linux")))]
    {
        // Some macOS sandbox profiles reject creation of non-UTF-8 pathnames.
        // A multibyte native OS string still verifies that lookup does not
        // narrow candidates to ASCII or hard-coded C strings.
        let native_name = OsStr::new("ssh-💡");
        let native = create_candidate(root.path(), native_name, true);
        let resolved = resolve_ssh_path(SshPathOptions {
            cli: Some(native.as_os_str()),
            env: None,
            path: None,
        })
        .unwrap();
        assert_eq!(resolved, native);
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;

        let recursive_dir = root.path().join("recursive");
        fs::create_dir(&recursive_dir).unwrap();
        symlink(std::env::current_exe().unwrap(), recursive_dir.join("ssh")).unwrap();
        let recursive_path = std::env::join_paths([recursive_dir]).unwrap();
        assert!(matches!(
            resolve_ssh_path(SshPathOptions {
                cli: None,
                env: None,
                path: Some(recursive_path.as_os_str()),
            }),
            Err(ResolveError::NotFoundInPath { .. })
        ));

        let hard_link_dir = root.path().join("hard-link-recursion");
        fs::create_dir(&hard_link_dir).unwrap();
        let explicit_hard_link = hard_link_dir.join("explicit-ssh");
        fs::hard_link(std::env::current_exe().unwrap(), &explicit_hard_link).unwrap();
        assert!(matches!(
            resolve_ssh_path(SshPathOptions {
                cli: Some(explicit_hard_link.as_os_str()),
                env: None,
                path: None,
            }),
            Err(ResolveError::RecursiveOverride { .. })
        ));

        let path_hard_link_dir = root.path().join("hard-link-path");
        fs::create_dir(&path_hard_link_dir).unwrap();
        fs::hard_link(
            std::env::current_exe().unwrap(),
            path_hard_link_dir.join("ssh"),
        )
        .unwrap();
        let hard_link_path = std::env::join_paths([path_hard_link_dir]).unwrap();
        assert!(matches!(
            resolve_ssh_path(SshPathOptions {
                cli: None,
                env: None,
                path: Some(hard_link_path.as_os_str()),
            }),
            Err(ResolveError::NotFoundInPath { .. })
        ));
    }

    let ambient_output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "environment_helper_uses_shue_ssh_without_mutating_parent_environment",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("SHUE_SSH", &env)
        .env("PATH", "")
        .output()
        .unwrap();
    assert!(
        ambient_output.status.success(),
        "environment helper failed: {}",
        String::from_utf8_lossy(&ambient_output.stderr)
    );
    assert!(
        ambient_output
            .stdout
            .windows(b"ambient SHUE_SSH passed".len())
            .any(|part| part == b"ambient SHUE_SSH passed")
    );

    #[cfg(unix)]
    verify_symlink_invocation_pinning(root.path());

    println!("SSH resolution verification passed");
}

#[test]
#[ignore = "launched in an isolated environment by the parent verification test"]
fn environment_helper_uses_shue_ssh_without_mutating_parent_environment() {
    let expected = std::env::var_os("SHUE_SSH").unwrap();
    assert_eq!(
        resolve_ssh_path_from_env(None).unwrap(),
        std::path::PathBuf::from(expected)
    );
    println!("ambient SHUE_SSH passed");
}

#[test]
#[cfg(unix)]
#[ignore = "launched in an isolated working directory by the parent verification test"]
fn relative_candidates_are_pinned_before_the_caller_can_change_path() {
    let malicious_path = std::env::var_os("SHUE_TEST_MALICIOUS_PATH").unwrap();
    let alternate_cwd = std::env::var_os("SHUE_TEST_ALTERNATE_CWD").unwrap();

    let explicit = resolve_ssh_path(SshPathOptions {
        cli: None,
        env: Some(OsStr::new("selected-ssh")),
        path: None,
    })
    .unwrap();
    assert!(explicit.is_absolute());
    assert_eq!(explicit.file_name(), Some(OsStr::new("selected-ssh")));
    assert_ne!(explicit, fs::canonicalize("selected-ssh").unwrap());

    let relative_path = resolve_ssh_path(SshPathOptions {
        cli: None,
        env: None,
        path: Some(OsStr::new("relative-bin")),
    })
    .unwrap();
    assert!(relative_path.is_absolute());
    assert_eq!(relative_path.file_name(), Some(OsStr::new("ssh")));
    assert_ne!(relative_path, fs::canonicalize("relative-bin/ssh").unwrap());

    let path_with_empty_entry = std::env::join_paths([Path::new(""), Path::new("missing")])
        .expect("test PATH must be representable");
    let empty_entry = resolve_ssh_path(SshPathOptions {
        cli: None,
        env: None,
        path: Some(path_with_empty_entry.as_os_str()),
    })
    .unwrap();
    assert!(empty_entry.is_absolute());
    assert_eq!(empty_entry.file_name(), Some(OsStr::new("ssh")));
    assert_ne!(empty_entry, fs::canonicalize("ssh").unwrap());

    std::env::set_current_dir(alternate_cwd).unwrap();
    assert_candidate_output(&explicit, &malicious_path, b"selected-ssh\n");
    assert_candidate_output(&relative_path, &malicious_path, b"ssh\n");
    assert_candidate_output(&empty_entry, &malicious_path, b"ssh\n");
    assert_pty_candidate_output(&explicit, b"selected-ssh");
    assert_pty_candidate_output(&relative_path, b"ssh");

    println!("relative path pinning passed");
}

fn assert_candidate_output(candidate: &Path, malicious_path: &OsStr, expected: &[u8]) {
    let output = Command::new(candidate)
        .env("PATH", malicious_path)
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, expected);
    assert_ne!(output.stdout, b"redirected\n");
}

#[cfg(unix)]
fn assert_pty_candidate_output(candidate: &Path, expected_basename: &[u8]) {
    let mut session =
        PtySession::spawn(candidate.as_os_str(), &[], TerminalSize::default()).unwrap();
    let mut reader = session.try_clone_reader().unwrap();
    let mut output = Vec::new();
    reader.read_to_end(&mut output).unwrap();
    assert_eq!(session.wait().unwrap(), 0);
    assert!(
        output
            .windows(expected_basename.len())
            .any(|part| part == expected_basename),
        "PTY changed the invocation basename: {}",
        String::from_utf8_lossy(&output)
    );
    assert!(
        !output
            .windows(b"redirected".len())
            .any(|part| part == b"redirected")
    );
}

#[cfg(unix)]
fn verify_symlink_invocation_pinning(root: &Path) {
    use std::os::unix::fs::symlink;

    let pinning_root = root.join("pinning");
    let malicious_dir = root.join("malicious-path");
    let alternate_cwd = root.join("alternate-cwd");
    let relative_bin = pinning_root.join("relative-bin");
    fs::create_dir(&pinning_root).unwrap();
    fs::create_dir(&malicious_dir).unwrap();
    fs::create_dir(&alternate_cwd).unwrap();
    fs::create_dir(&relative_bin).unwrap();

    let dispatch_target = pinning_root.join("multicall-target");
    fs::write(
        &dispatch_target,
        b"#!/bin/sh\nprintf '%s\\n' \"${0##*/}\"\n",
    )
    .unwrap();
    set_executable(&dispatch_target, true);
    symlink("multicall-target", pinning_root.join("selected-ssh")).unwrap();
    symlink("multicall-target", pinning_root.join("ssh")).unwrap();
    symlink("../multicall-target", relative_bin.join("ssh")).unwrap();

    create_output_candidate(&malicious_dir, OsStr::new("selected-ssh"), "redirected");
    create_output_candidate(&malicious_dir, OsStr::new("ssh"), "redirected");
    create_output_candidate(&alternate_cwd, OsStr::new("selected-ssh"), "redirected");
    create_output_candidate(&alternate_cwd, OsStr::new("ssh"), "redirected");
    let alternate_relative_bin = alternate_cwd.join("relative-bin");
    fs::create_dir(&alternate_relative_bin).unwrap();
    create_output_candidate(&alternate_relative_bin, OsStr::new("ssh"), "redirected");

    let pinning_output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "relative_candidates_are_pinned_before_the_caller_can_change_path",
            "--nocapture",
            "--test-threads=1",
        ])
        .current_dir(&pinning_root)
        .env("PATH", &malicious_dir)
        .env("SHUE_TEST_MALICIOUS_PATH", &malicious_dir)
        .env("SHUE_TEST_ALTERNATE_CWD", &alternate_cwd)
        .output()
        .unwrap();
    assert!(
        pinning_output.status.success(),
        "path-pinning helper failed: {}",
        String::from_utf8_lossy(&pinning_output.stderr)
    );
    assert!(
        pinning_output
            .stdout
            .windows(b"relative path pinning passed".len())
            .any(|part| part == b"relative path pinning passed")
    );
}
