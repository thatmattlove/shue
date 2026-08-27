use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};

use thiserror::Error;

/// Useful, terminal-theme-aware rules used only when no config file exists.
///
/// These patterns are original to shue. They intentionally use native ANSI
/// names so the terminal's own palette remains authoritative.
pub const EMBEDDED_DEFAULT_CONFIG: &str = r#"palette: {}
rules:
  - description: Error and failure states
    regex: '(?i)\b(?:error|failed|failure|fatal|denied|down)\b'
    color: 'fg:red bold'
    exclusive: true
  - description: Warning and transitional states
    regex: '(?i)\b(?:warn(?:ing)?|degraded|pending|retry(?:ing)?)\b'
    color: 'fg:yellow bold'
  - description: Healthy states
    regex: '(?i)\b(?:ok|online|up|passed|success(?:ful(?:ly)?)?)\b'
    color: 'fg:green bold'
  - description: IPv4 addresses
    regex: '\b(?:(?:25[0-5]|2[0-4]\d|1?\d?\d)\.){3}(?:25[0-5]|2[0-4]\d|1?\d?\d)\b'
    color: 'fg:cyan'
  - description: Common IPv6 forms
    regex: '(?<![0-9A-Fa-f:])(?:(?:[0-9A-Fa-f]{1,4}:){7}[0-9A-Fa-f]{1,4}|(?:[0-9A-Fa-f]{1,4}:){1,7}:|(?:[0-9A-Fa-f]{1,4}:){1,6}:[0-9A-Fa-f]{1,4}|::1|::)(?![0-9A-Fa-f:])'
    color: 'fg:cyan'
  - description: HTTP and HTTPS URLs
    regex: '\bhttps?://[^[:space:]<>"]+'
    color: 'fg:blue underline'
  - description: Standalone numbers
    regex: '(?<![\w.])-?\d+(?:\.\d+)?(?![\w.])'
    color: 'fg:magenta'
"#;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConfigOrigin {
    Cli(PathBuf),
    Environment(PathBuf),
    Discovered(PathBuf),
    Embedded,
}

impl ConfigOrigin {
    pub fn label(&self) -> String {
        match self {
            Self::Cli(path) => format!("CLI config {}", path.display()),
            Self::Environment(path) => format!("SHUE_CONFIG {}", path.display()),
            Self::Discovered(path) => format!("config {}", path.display()),
            Self::Embedded => "embedded defaults".to_owned(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoadedConfig {
    pub contents: String,
    pub origin: ConfigOrigin,
}

/// Inputs used to build deterministic config search paths.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfigSearch {
    pub user_config_dir: Option<PathBuf>,
    pub system_config_dirs: Vec<PathBuf>,
}

impl ConfigSearch {
    pub fn from_environment() -> Self {
        let home = nonempty_var_os("HOME").map(PathBuf::from);
        let user_config_dir = nonempty_var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .or_else(|| home.as_ref().map(|home| home.join(".config")));

        let mut system_config_dirs = nonempty_var_os("XDG_CONFIG_DIRS")
            .map(|paths| {
                env::split_paths(&paths)
                    .filter(|path| path.is_absolute())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_else(|| vec![PathBuf::from("/etc/xdg")]);
        if !system_config_dirs
            .iter()
            .any(|path| path == Path::new("/etc"))
        {
            system_config_dirs.push(PathBuf::from("/etc"));
        }

        Self {
            user_config_dir,
            system_config_dirs,
        }
    }

    /// Ordered discovery candidates after CLI and `SHUE_CONFIG` overrides.
    pub fn candidates(&self) -> Vec<PathBuf> {
        let mut candidates = Vec::new();

        if let Some(config_dir) = &self.user_config_dir {
            push_unique(&mut candidates, config_dir.join("shue/config.yaml"));
            push_unique(&mut candidates, config_dir.join("shue/config.yml"));
        }
        for config_dir in &self.system_config_dirs {
            push_unique(&mut candidates, config_dir.join("shue/config.yaml"));
            push_unique(&mut candidates, config_dir.join("shue/config.yml"));
        }
        candidates
    }
}

fn push_unique(paths: &mut Vec<PathBuf>, candidate: PathBuf) {
    if !paths.contains(&candidate) {
        paths.push(candidate);
    }
}

fn nonempty_var_os(name: &str) -> Option<OsString> {
    env::var_os(name).filter(|value| !value.is_empty())
}

#[derive(Debug, Error)]
pub enum ConfigLoadError {
    #[error("unable to read {origin}: {source}")]
    Read {
        origin: String,
        #[source]
        source: std::io::Error,
    },
    #[error("{origin} is not valid UTF-8")]
    NotUtf8 { origin: String },
}

/// Load the first config according to the documented precedence.
pub fn load_config(
    cli_path: Option<&Path>,
    environment_path: Option<&OsStr>,
    search: &ConfigSearch,
) -> Result<LoadedConfig, ConfigLoadError> {
    if let Some(path) = cli_path {
        return read_required(path, ConfigOrigin::Cli(path.to_owned()));
    }
    if let Some(path) = environment_path.filter(|path| !path.is_empty()) {
        let path = PathBuf::from(path);
        return read_required(&path, ConfigOrigin::Environment(path.clone()));
    }

    for path in search.candidates() {
        match fs::read(&path) {
            Ok(contents) => {
                let origin = ConfigOrigin::Discovered(path);
                return decode(contents, origin);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(ConfigLoadError::Read {
                    origin: format!("config {}", path.display()),
                    source,
                });
            }
        }
    }

    Ok(LoadedConfig {
        contents: EMBEDDED_DEFAULT_CONFIG.to_owned(),
        origin: ConfigOrigin::Embedded,
    })
}

fn read_required(path: &Path, origin: ConfigOrigin) -> Result<LoadedConfig, ConfigLoadError> {
    let label = origin.label();
    let contents = fs::read(path).map_err(|source| ConfigLoadError::Read {
        origin: label,
        source,
    })?;
    decode(contents, origin)
}

fn decode(contents: Vec<u8>, origin: ConfigOrigin) -> Result<LoadedConfig, ConfigLoadError> {
    let label = origin.label();
    let contents =
        String::from_utf8(contents).map_err(|_| ConfigLoadError::NotUtf8 { origin: label })?;
    Ok(LoadedConfig { contents, origin })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidates_have_native_user_and_system_order() {
        let search = ConfigSearch {
            user_config_dir: Some(PathBuf::from("/xdg/user")),
            system_config_dirs: vec![PathBuf::from("/xdg/system"), PathBuf::from("/etc")],
        };
        assert_eq!(
            search.candidates(),
            vec![
                PathBuf::from("/xdg/user/shue/config.yaml"),
                PathBuf::from("/xdg/user/shue/config.yml"),
                PathBuf::from("/xdg/system/shue/config.yaml"),
                PathBuf::from("/xdg/system/shue/config.yml"),
                PathBuf::from("/etc/shue/config.yaml"),
                PathBuf::from("/etc/shue/config.yml"),
            ]
        );
    }
}
