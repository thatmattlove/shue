use std::env;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use thiserror::Error;

/// Explicit inputs to SSH executable discovery.
///
/// `env` is the already-read value of `SHUE_SSH`, while `path` is the
/// already-read value of `PATH`. Keeping those values explicit makes lookup
/// deterministic and avoids changing process-global environment state in
/// tests. Use [`resolve_ssh_path_from_env`] for normal process-environment
/// discovery.
#[derive(Clone, Copy, Debug, Default)]
pub struct SshPathOptions<'a> {
    pub cli: Option<&'a OsStr>,
    pub env: Option<&'a OsStr>,
    pub path: Option<&'a OsStr>,
}

/// The explicit setting which selected an SSH candidate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SshPathSource {
    Cli,
    Environment,
}

impl fmt::Display for SshPathSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cli => formatter.write_str("--ssh-path"),
            Self::Environment => formatter.write_str("SHUE_SSH"),
        }
    }
}

/// A failure to resolve an executable SSH client.
#[derive(Debug, Error)]
pub enum ResolveError {
    #[error("{origin} is set but empty")]
    EmptyOverride { origin: SshPathSource },

    #[error("{origin} points to `{path}`, but that path does not exist")]
    OverrideNotFound {
        origin: SshPathSource,
        path: PathBuf,
    },

    #[error("cannot inspect {origin} candidate `{path}`: {error}")]
    OverrideUnreadable {
        origin: SshPathSource,
        path: PathBuf,
        #[source]
        error: io::Error,
    },

    #[error("{origin} candidate `{path}` is not a regular file")]
    OverrideNotFile {
        origin: SshPathSource,
        path: PathBuf,
    },

    #[error("{origin} candidate `{path}` is not executable")]
    OverrideNotExecutable {
        origin: SshPathSource,
        path: PathBuf,
    },

    #[error(
        "{origin} candidate `{path}` resolves to the running shue executable; refusing recursive execution"
    )]
    RecursiveOverride {
        origin: SshPathSource,
        path: PathBuf,
    },

    #[error("PATH is not set or is empty; provide --ssh-path or SHUE_SSH")]
    PathNotSet,

    #[error(
        "could not find an executable `ssh` in PATH after searching {searched} entries; rejected candidates: {rejected:?}"
    )]
    NotFoundInPath {
        searched: usize,
        rejected: Vec<PathBuf>,
    },
}

/// Resolve the SSH executable using CLI, environment override, then `PATH`.
///
/// An explicitly supplied invalid override is an error and never silently
/// falls through to a lower-priority source. This catches misspellings and
/// stale configuration early.
pub fn resolve_ssh_path(options: SshPathOptions<'_>) -> Result<PathBuf, ResolveError> {
    if let Some(candidate) = options.cli {
        return validate_override(candidate, SshPathSource::Cli);
    }

    if let Some(candidate) = options.env {
        return validate_override(candidate, SshPathSource::Environment);
    }

    let path = options
        .path
        .filter(|value| !value.is_empty())
        .ok_or(ResolveError::PathNotSet)?;
    search_path(path)
}

/// Resolve SSH from an optional CLI override and this process's environment.
pub fn resolve_ssh_path_from_env(cli: Option<&OsStr>) -> Result<PathBuf, ResolveError> {
    let env_override = env::var_os("SHUE_SSH");
    let search_path = env::var_os("PATH");

    resolve_ssh_path(SshPathOptions {
        cli,
        env: env_override.as_deref(),
        path: search_path.as_deref(),
    })
}

fn validate_override(candidate: &OsStr, origin: SshPathSource) -> Result<PathBuf, ResolveError> {
    if candidate.is_empty() {
        return Err(ResolveError::EmptyOverride { origin });
    }

    let path = PathBuf::from(candidate);
    let pinned =
        absolute_invocation_path(&path).map_err(|error| ResolveError::OverrideUnreadable {
            origin,
            path: path.clone(),
            error,
        })?;
    let metadata = fs::metadata(&pinned).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            ResolveError::OverrideNotFound {
                origin,
                path: path.clone(),
            }
        } else {
            ResolveError::OverrideUnreadable {
                origin,
                path: path.clone(),
                error,
            }
        }
    })?;

    if !metadata.is_file() {
        return Err(ResolveError::OverrideNotFile { origin, path });
    }
    if !is_executable(&pinned, &metadata) {
        return Err(ResolveError::OverrideNotExecutable { origin, path });
    }
    if is_current_executable(&pinned) {
        return Err(ResolveError::RecursiveOverride { origin, path });
    }

    Ok(pinned)
}

fn search_path(path: &OsStr) -> Result<PathBuf, ResolveError> {
    let mut searched = 0;
    let mut rejected = Vec::new();
    let executable_names = ssh_executable_names();

    for directory in env::split_paths(path) {
        searched += 1;
        for executable_name in &executable_names {
            let candidate = directory.join(executable_name);
            let Ok(pinned) = absolute_invocation_path(&candidate) else {
                rejected.push(candidate);
                continue;
            };
            match fs::metadata(&pinned) {
                Ok(metadata)
                    if metadata.is_file()
                        && is_executable(&pinned, &metadata)
                        && !is_current_executable(&pinned) =>
                {
                    return Ok(pinned);
                }
                Ok(_) => rejected.push(candidate),
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                    ) => {}
                Err(_) => rejected.push(candidate),
            }
        }
    }

    Err(ResolveError::NotFoundInPath { searched, rejected })
}

/// Make a candidate independent of subsequent working-directory and PATH
/// changes without resolving its final symlink. Preserving that final path
/// component is required by multicall binaries which dispatch on `argv[0]`.
fn absolute_invocation_path(path: &Path) -> io::Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        env::current_dir().map(|directory| directory.join(path))
    }
}

#[cfg(unix)]
fn ssh_executable_names() -> Vec<OsString> {
    vec![OsString::from("ssh")]
}

#[cfg(windows)]
fn ssh_executable_names() -> Vec<OsString> {
    let mut names = vec![OsString::from("ssh")];
    let extensions =
        env::var_os("PATHEXT").unwrap_or_else(|| OsString::from(".COM;.EXE;.BAT;.CMD"));
    for extension in extensions.to_string_lossy().split(';') {
        if extension.is_empty() {
            continue;
        }
        let mut name = OsString::from("ssh");
        name.push(extension);
        names.push(name);
    }
    names
}

#[cfg(not(any(unix, windows)))]
fn ssh_executable_names() -> Vec<OsString> {
    vec![OsString::from("ssh")]
}

#[cfg(unix)]
fn is_executable(path: &Path, metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;

    if metadata.permissions().mode() & 0o111 == 0 {
        return false;
    }

    // Mode bits alone are insufficient: if the current user owns a file, an
    // execute bit set only for "other" does not grant that owner access. Ask
    // the OS using effective uid/gid semantics so ACLs and identity selection
    // are also accounted for.
    rustix::fs::accessat(
        rustix::fs::CWD,
        path,
        rustix::fs::Access::EXEC_OK,
        rustix::fs::AtFlags::EACCESS,
    )
    .is_ok()
}

#[cfg(windows)]
fn is_executable(path: &Path, _metadata: &fs::Metadata) -> bool {
    let Some(extension) = path.extension() else {
        return false;
    };
    let extension = format!(".{}", extension.to_string_lossy());
    let path_extensions =
        env::var_os("PATHEXT").unwrap_or_else(|| OsString::from(".COM;.EXE;.BAT;.CMD"));
    path_extensions
        .to_string_lossy()
        .split(';')
        .any(|candidate| candidate.eq_ignore_ascii_case(&extension))
}

#[cfg(not(any(unix, windows)))]
fn is_executable(_path: &Path, _metadata: &fs::Metadata) -> bool {
    true
}

#[cfg(unix)]
fn is_current_executable(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;

    let Ok(current) = env::current_exe() else {
        return false;
    };
    match (fs::metadata(path), fs::metadata(&current)) {
        (Ok(candidate), Ok(running)) => {
            candidate.dev() == running.dev() && candidate.ino() == running.ino()
        }
        _ => canonical_paths_match(path, &current),
    }
}

#[cfg(not(unix))]
fn is_current_executable(path: &Path) -> bool {
    let Ok(current) = env::current_exe() else {
        return false;
    };
    canonical_paths_match(path, &current)
}

fn canonical_paths_match(left: &Path, right: &Path) -> bool {
    match (fs::canonicalize(left), fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}
