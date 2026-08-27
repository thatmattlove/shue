use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::PathBuf;

/// Color capability requested by the user.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ColorDepthChoice {
    Auto,
    Ansi16,
    Ansi256,
    TrueColor,
}

impl ColorDepthChoice {
    pub fn parse(value: &OsStr) -> Result<Self, CliError> {
        match value.to_str() {
            Some("auto") => Ok(Self::Auto),
            Some("ansi16") => Ok(Self::Ansi16),
            Some("ansi256") => Ok(Self::Ansi256),
            Some("truecolor") => Ok(Self::TrueColor),
            _ => Err(CliError::InvalidColorDepth(value.to_os_string())),
        }
    }
}

/// Selected execution mode.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Mode {
    /// Run the discovered SSH binary. Arguments are kept as `OsString`s.
    Ssh { args: Vec<OsString> },
    /// Run an explicitly selected program. Everything after the program is
    /// passed through without any further `shue` parsing.
    Exec {
        program: OsString,
        args: Vec<OsString>,
    },
    /// Highlight standard input and write it to standard output.
    Filter,
}

/// Fully parsed command line.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Cli {
    pub mode: Mode,
    pub ssh_path: Option<PathBuf>,
    pub config_path: Option<PathBuf>,
    pub color_depth: Option<ColorDepthChoice>,
    pub no_color: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Informational {
    Help,
    Version,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParseResult {
    Run(Cli),
    Informational(Informational),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CliError {
    MissingValue(&'static str),
    MissingProgram,
    InvalidColorDepth(OsString),
    ConflictingModes,
    FilterArgument(OsString),
    SshPathOutsideSshMode,
}

impl std::error::Error for CliError {}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingValue(option) => write!(f, "{option} requires a value"),
            Self::MissingProgram => write!(f, "--exec requires a program"),
            Self::InvalidColorDepth(value) => write!(
                f,
                "invalid color depth {:?}; expected auto, ansi16, ansi256, or truecolor",
                value
            ),
            Self::ConflictingModes => write!(f, "--filter and --exec cannot be used together"),
            Self::FilterArgument(argument) => {
                write!(
                    f,
                    "--filter does not accept a program argument: {argument:?}"
                )
            }
            Self::SshPathOutsideSshMode => {
                write!(f, "--ssh-path is only valid in the default SSH mode")
            }
        }
    }
}

/// Parse `shue` arguments without converting them to UTF-8.
///
/// Shue options are recognized only before the first positional or unrecognized
/// token. That boundary is what makes `shue host command --config value` safe:
/// the remote command is forwarded in full.
pub fn parse<I>(arguments: I) -> Result<ParseResult, CliError>
where
    I: IntoIterator<Item = OsString>,
{
    let mut arguments = arguments.into_iter().peekable();
    let mut ssh_path = None;
    let mut config_path = None;
    let mut color_depth = None;
    let mut no_color = false;
    let mut filter = false;
    let mut ssh_args = Vec::new();

    while let Some(argument) = arguments.next() {
        if argument == OsStr::new("--") {
            let remainder: Vec<_> = arguments.collect();
            if filter {
                if let Some(argument) = remainder.into_iter().next() {
                    return Err(CliError::FilterArgument(argument));
                }
            } else {
                ssh_args.extend(remainder);
            }
            break;
        }

        if argument == OsStr::new("--help") {
            return Ok(ParseResult::Informational(Informational::Help));
        }
        if argument == OsStr::new("--version") {
            return Ok(ParseResult::Informational(Informational::Version));
        }
        if argument == OsStr::new("--no-color") {
            no_color = true;
            continue;
        }
        if argument == OsStr::new("--filter") {
            if filter {
                continue;
            }
            filter = true;
            continue;
        }

        if let Some(value) = long_value(&argument, "--ssh-path") {
            ssh_path = Some(PathBuf::from(value?));
            continue;
        }
        if argument == OsStr::new("--ssh-path") {
            ssh_path = Some(PathBuf::from(
                arguments
                    .next()
                    .ok_or(CliError::MissingValue("--ssh-path"))?,
            ));
            continue;
        }

        if let Some(value) = long_value(&argument, "--config") {
            config_path = Some(PathBuf::from(value?));
            continue;
        }
        if argument == OsStr::new("--config") {
            config_path = Some(PathBuf::from(
                arguments.next().ok_or(CliError::MissingValue("--config"))?,
            ));
            continue;
        }

        if let Some(value) = long_value(&argument, "--color-depth") {
            color_depth = Some(ColorDepthChoice::parse(&value?)?);
            continue;
        }
        if argument == OsStr::new("--color-depth") {
            let value = arguments
                .next()
                .ok_or(CliError::MissingValue("--color-depth"))?;
            color_depth = Some(ColorDepthChoice::parse(&value)?);
            continue;
        }

        if let Some(value) = long_value(&argument, "--exec") {
            if filter {
                return Err(CliError::ConflictingModes);
            }
            if ssh_path.is_some() {
                return Err(CliError::SshPathOutsideSshMode);
            }
            let program = value?;
            if program.is_empty() {
                return Err(CliError::MissingProgram);
            }
            return Ok(ParseResult::Run(Cli {
                mode: Mode::Exec {
                    program,
                    args: arguments.collect(),
                },
                ssh_path,
                config_path,
                color_depth,
                no_color,
            }));
        }
        if argument == OsStr::new("--exec") {
            if filter {
                return Err(CliError::ConflictingModes);
            }
            if ssh_path.is_some() {
                return Err(CliError::SshPathOutsideSshMode);
            }
            let program = arguments.next().ok_or(CliError::MissingProgram)?;
            return Ok(ParseResult::Run(Cli {
                mode: Mode::Exec {
                    program,
                    args: arguments.collect(),
                },
                ssh_path,
                config_path,
                color_depth,
                no_color,
            }));
        }

        // The first token that is not a recognized shue option is an SSH token.
        // Forward it and every remaining argument exactly as supplied.
        if filter {
            return Err(CliError::FilterArgument(argument));
        }
        ssh_args.push(argument);
        ssh_args.extend(arguments);
        break;
    }

    let mode = if filter {
        if ssh_path.is_some() {
            return Err(CliError::SshPathOutsideSshMode);
        }
        Mode::Filter
    } else {
        Mode::Ssh { args: ssh_args }
    };

    Ok(ParseResult::Run(Cli {
        mode,
        ssh_path,
        config_path,
        color_depth,
        no_color,
    }))
}

fn long_value(argument: &OsStr, option: &'static str) -> Option<Result<OsString, CliError>> {
    let value = argument.to_str()?;
    let prefix = format!("{option}=");
    value.strip_prefix(&prefix).map(|value| {
        if value.is_empty() {
            Err(CliError::MissingValue(option))
        } else {
            Ok(OsString::from(value))
        }
    })
}

pub const HELP: &str = r#"shue — fast PCRE2 highlighting for SSH and other terminal programs

Usage:
  shue [SHUE_OPTIONS] [username@host [SSH_ARGUMENTS...]]
  shue [SHUE_OPTIONS] --exec PROGRAM [ARGUMENTS...]
  shue [SHUE_OPTIONS] --filter

Modes:
      (default)             Run SSH; `shue username@host` needs no shell alias
      --exec PROGRAM        Wrap PROGRAM; remaining arguments pass through
      --filter              Highlight standard input to standard output

Shue options (put these before the first SSH argument):
      --ssh-path PATH       SSH executable (overrides SHUE_SSH and PATH lookup)
      --config PATH         YAML rules (overrides SHUE_CONFIG and discovery)
      --color-depth DEPTH   auto, ansi16, ansi256, or truecolor
      --no-color            Preserve bytes without adding highlighting
      --help                Print this help
      --version             Print the version

`--` ends shue option parsing. The first unrecognized or positional token also
ends parsing, so SSH options and remote-command arguments are forwarded without
reinterpretation. SSH discovery order: --ssh-path, SHUE_SSH, then PATH.
NO_COLOR disables added color; SHUE_COLOR_DEPTH sets the depth when the CLI
option is absent.
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn stops_parsing_at_first_ssh_token() {
        let result = parse(strings(&[
            "--color-depth",
            "ansi16",
            "host",
            "remote-command",
            "--config",
            "remote-file",
        ]))
        .unwrap();
        let ParseResult::Run(cli) = result else {
            panic!("expected run result")
        };
        assert_eq!(
            cli.mode,
            Mode::Ssh {
                args: strings(&["host", "remote-command", "--config", "remote-file"])
            }
        );
        assert_eq!(cli.config_path, None);
        assert_eq!(cli.color_depth, Some(ColorDepthChoice::Ansi16));
    }

    #[test]
    fn delimiter_forwards_colliding_long_option() {
        let result = parse(strings(&["--", "--filter", "host"])).unwrap();
        let ParseResult::Run(cli) = result else {
            panic!("expected run result")
        };
        assert_eq!(
            cli.mode,
            Mode::Ssh {
                args: strings(&["--filter", "host"])
            }
        );
    }

    #[test]
    fn exec_is_a_hard_passthrough_boundary() {
        let result = parse(strings(&[
            "--config",
            "rules.yml",
            "--exec",
            "tool",
            "--filter",
            "--color-depth",
            "bad-value",
        ]))
        .unwrap();
        let ParseResult::Run(cli) = result else {
            panic!("expected run result")
        };
        assert_eq!(
            cli.mode,
            Mode::Exec {
                program: OsString::from("tool"),
                args: strings(&["--filter", "--color-depth", "bad-value"])
            }
        );
    }

    #[cfg(unix)]
    #[test]
    fn preserves_non_utf8_ssh_arguments() {
        use std::os::unix::ffi::OsStringExt;

        let raw = OsString::from_vec(vec![b'h', b'o', 0xff, b't']);
        let result = parse(vec![raw.clone(), OsString::from("--config")]).unwrap();
        let ParseResult::Run(cli) = result else {
            panic!("expected run result")
        };
        assert_eq!(
            cli.mode,
            Mode::Ssh {
                args: vec![raw, OsString::from("--config")]
            }
        );
    }
}
