use std::env;
use std::ffi::{OsStr, OsString};
use std::io::{self, BufWriter, IsTerminal, Read, Write};
use std::process::{Child, Command, ExitCode, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use shue::cli::{self, Cli, ColorDepthChoice, Informational, Mode, ParseResult};
use shue::config::{ConfigSearch, load_config};
use shue_core::{ColorDepth, Config, StreamHighlighter};
use shue_runtime::{PtySession, RawModeGuard, SshPathOptions, TerminalSize, resolve_ssh_path};

const READ_BUFFER_SIZE: usize = 32 * 1024;
const OUTPUT_BUFFER_SIZE: usize = 128 * 1024;
const PTY_QUEUE_DEPTH: usize = 8;
const PROMPT_IDLE_FLUSH: Duration = Duration::from_millis(20);
const DISPLAY_FLUSH_INTERVAL: Duration = Duration::from_millis(16);
const CHILD_TERMINATION_GRACE: Duration = Duration::from_millis(250);

/// Owns a noninteractive child until it has been reaped.
///
/// When standard input is not a terminal, the Unix child leads a dedicated
/// process group. Cleanup then signals the group so helpers spawned by SSH or
/// an explicit program do not outlive a failed or terminated wrapper. A child
/// reading terminal input stays in the wrapper's foreground process group.
struct ChildGuard {
    child: Child,
    process_group: Option<i32>,
    reaped: bool,
}

impl ChildGuard {
    fn new(child: Child, isolated_process_group: bool) -> Self {
        let process_group = if isolated_process_group {
            i32::try_from(child.id()).ok()
        } else {
            None
        };
        Self {
            child,
            process_group,
            reaped: false,
        }
    }

    fn take_stdout(&mut self) -> Option<std::process::ChildStdout> {
        self.child.stdout.take()
    }

    fn wait(&mut self) -> io::Result<ExitStatus> {
        let status = self.child.wait()?;
        self.reaped = true;
        Ok(status)
    }

    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        let status = self.child.try_wait()?;
        if status.is_some() {
            self.reaped = true;
        }
        Ok(status)
    }

    #[cfg(unix)]
    fn send_signal(&self, raw_signal: i32) -> io::Result<()> {
        use nix::errno::Errno;
        use nix::sys::signal::{Signal, kill, killpg};
        use nix::unistd::Pid;

        let signal = Signal::try_from(raw_signal)
            .map_err(|error| io::Error::from_raw_os_error(error as i32))?;
        let result = match self.process_group {
            Some(group) => killpg(Pid::from_raw(group), signal),
            None => kill(Pid::from_raw(self.child.id() as i32), signal),
        };
        match result {
            Ok(()) | Err(Errno::ESRCH) => Ok(()),
            Err(error) => Err(io::Error::from_raw_os_error(error as i32)),
        }
    }

    #[cfg(not(unix))]
    fn send_signal(&mut self, _raw_signal: i32) -> io::Result<()> {
        self.child.kill()
    }

    fn terminate_and_reap(&mut self, signal: i32) -> Result<()> {
        self.send_signal(signal)
            .context("unable to forward the termination signal to the child process group")?;
        // Give ordinary signal handlers a bounded opportunity to clean up,
        // then kill the entire group while its unreaped leader still prevents
        // process-group-id reuse.
        thread::sleep(CHILD_TERMINATION_GRACE);
        let force_result = self.send_signal(signal_hook::consts::SIGKILL);
        match self.child.kill() {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::InvalidInput => {}
            Err(error) if force_result.is_ok() => {
                let _ = error;
            }
            Err(error) => return Err(error).context("unable to kill the child process"),
        }
        self.wait().context("unable to reap the terminated child")?;
        Ok(())
    }

    fn force_cleanup(&mut self) {
        if self.reaped {
            return;
        }
        let _ = self.send_signal(signal_hook::consts::SIGKILL);
        let _ = self.child.kill();
        if self.child.wait().is_ok() {
            self.reaped = true;
        }
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        self.force_cleanup();
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => exit_code(code),
        Err(error) if is_broken_pipe(&error) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("shue: {}", concise_error(&error));
            ExitCode::FAILURE
        }
    }
}

fn exit_code(code: u32) -> ExitCode {
    match u8::try_from(code) {
        Ok(code) => ExitCode::from(code),
        Err(_) => ExitCode::FAILURE,
    }
}

fn run() -> Result<u32> {
    let parsed = cli::parse(env::args_os().skip(1)).map_err(anyhow::Error::new)?;
    let cli = match parsed {
        ParseResult::Informational(Informational::Help) => {
            print!("{}", cli::HELP);
            return Ok(0);
        }
        ParseResult::Informational(Informational::Version) => {
            println!("shue {}", env!("CARGO_PKG_VERSION"));
            return Ok(0);
        }
        ParseResult::Run(cli) => cli,
    };

    run_cli(cli)
}

fn run_cli(cli: Cli) -> Result<u32> {
    let depth = select_color_depth(cli.color_depth)?;
    let environment_config = env::var_os("SHUE_CONFIG");
    let loaded = load_config(
        cli.config_path.as_deref(),
        environment_config.as_deref(),
        &ConfigSearch::from_environment(),
    )?;
    let (config, config_warnings) = Config::from_yaml_recovering(&loaded.contents, depth);
    for warning in config_warnings {
        eprintln!("shue: warning: {}: {warning}", loaded.origin.label());
    }
    let color_disabled = cli.no_color || env::var_os("NO_COLOR").is_some();
    let processor = if color_disabled {
        Processor::Plain
    } else {
        Processor::Highlight(StreamHighlighter::new(config))
    };

    match cli.mode {
        Mode::Filter => run_filter(processor),
        Mode::Exec { program, args } => run_program(&program, &args, processor),
        Mode::Ssh { args } => {
            let environment_ssh = env::var_os("SHUE_SSH");
            let environment_path = env::var_os("PATH");
            let program = resolve_ssh_path(SshPathOptions {
                cli: cli.ssh_path.as_deref().map(|path| path.as_os_str()),
                env: environment_ssh.as_deref(),
                path: environment_path.as_deref(),
            })
            .context("unable to locate the SSH executable")?;
            run_program(program.as_os_str(), &args, processor)
        }
    }
}

fn select_color_depth(cli_choice: Option<ColorDepthChoice>) -> Result<ColorDepth> {
    let choice = match cli_choice {
        Some(choice) => choice,
        None => match env::var_os("SHUE_COLOR_DEPTH") {
            Some(value) if !value.is_empty() => {
                ColorDepthChoice::parse(&value).map_err(anyhow::Error::new)?
            }
            _ => ColorDepthChoice::Auto,
        },
    };
    Ok(match choice {
        ColorDepthChoice::Auto => shue_core::detect_color_depth(),
        ColorDepthChoice::Ansi16 => ColorDepth::Ansi16,
        ColorDepthChoice::Ansi256 => ColorDepth::Ansi256,
        ColorDepthChoice::TrueColor => ColorDepth::TrueColor,
    })
}

enum Processor {
    Plain,
    Highlight(StreamHighlighter),
}

impl Processor {
    fn push(&mut self, input: &[u8], output: &mut Vec<u8>) -> Result<()> {
        match self {
            Self::Plain => output.extend_from_slice(input),
            Self::Highlight(highlighter) => {
                let result = highlighter.push(input, output);
                report_runtime_warnings(highlighter);
                result.context("highlighting output failed")?;
            }
        }
        Ok(())
    }

    fn flush(&mut self, output: &mut Vec<u8>) -> Result<()> {
        if let Self::Highlight(highlighter) = self {
            let result = highlighter.flush(output);
            report_runtime_warnings(highlighter);
            result.context("flushing highlighted output failed")?;
        }
        Ok(())
    }
}

fn report_runtime_warnings(highlighter: &StreamHighlighter) {
    for warning in highlighter.take_runtime_warnings() {
        eprintln!(
            "shue: warning: disabling rule {} after regex runtime error: {}",
            warning.rule.saturating_add(1),
            warning.message
        );
    }
}

fn run_filter(processor: Processor) -> Result<u32> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    stream_reader(stdin.lock(), stdout.lock(), processor)?;
    Ok(0)
}

fn run_program(program: &OsStr, args: &[OsString], processor: Processor) -> Result<u32> {
    if io::stdin().is_terminal() && io::stdout().is_terminal() {
        run_interactive(program, args, processor)
    } else {
        run_noninteractive(program, args, processor)
    }
}

fn run_noninteractive(program: &OsStr, args: &[OsString], processor: Processor) -> Result<u32> {
    let termination_signal = Arc::new(AtomicUsize::new(0));
    register_termination_flags(Arc::clone(&termination_signal))?;

    // A child inheriting terminal stdin must remain in the wrapper's foreground
    // process group or its first read can be stopped by SIGTTIN. When stdin is
    // not a terminal, isolation lets wrapper-directed signals clean up the
    // child and any descendants as one group.
    let isolate_child_group = !io::stdin().is_terminal();
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;

        if isolate_child_group {
            command.process_group(0);
        }
    }
    let child = command
        .spawn()
        .with_context(|| format!("unable to start {program:?}"))?;
    let mut child = ChildGuard::new(child, isolate_child_group);
    let stdout = child
        .take_stdout()
        .context("child stdout pipe was unavailable")?;
    let (events, recycle) = spawn_bounded_reader(stdout, "shue-pipe-output", false)?;
    let terminal_stdout = io::stdout();
    let mut terminal_stdout = BufWriter::with_capacity(OUTPUT_BUFFER_SIZE, terminal_stdout.lock());
    let mut processor = processor;
    let mut output = Vec::with_capacity(READ_BUFFER_SIZE + 1024);
    let mut dirty = false;

    loop {
        let signal = termination_signal.load(Ordering::Relaxed);
        if signal != 0 {
            child.terminate_and_reap(signal as i32)?;
            return Ok(128 + signal as u32);
        }

        match events.recv_timeout(PROMPT_IDLE_FLUSH) {
            Ok(ReadEvent::Data(input)) => {
                output.clear();
                processor.push(&input, &mut output)?;
                terminal_stdout
                    .write_all(&output)
                    .context("writing child output failed")?;
                terminal_stdout
                    .flush()
                    .context("flushing child output failed")?;
                dirty = true;
                let _ = recycle.try_send(input);
            }
            Ok(ReadEvent::Eof) => break,
            Ok(ReadEvent::Error(error)) => {
                return Err(error).context("reading child output failed");
            }
            Err(mpsc::RecvTimeoutError::Timeout) if dirty => {
                output.clear();
                processor.flush(&mut output)?;
                terminal_stdout
                    .write_all(&output)
                    .context("writing child output failed")?;
                terminal_stdout
                    .flush()
                    .context("flushing child output failed")?;
                dirty = false;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(anyhow::anyhow!("child output worker stopped unexpectedly"));
            }
        }
    }

    output.clear();
    processor.flush(&mut output)?;
    terminal_stdout
        .write_all(&output)
        .context("writing child output failed")?;
    terminal_stdout
        .flush()
        .context("flushing child output failed")?;
    let status = loop {
        let signal = termination_signal.load(Ordering::Relaxed);
        if signal != 0 {
            child.terminate_and_reap(signal as i32)?;
            return Ok(128 + signal as u32);
        }
        if let Some(status) = child
            .try_wait()
            .with_context(|| format!("unable to poll {program:?}"))?
        {
            break status;
        }
        thread::sleep(PROMPT_IDLE_FLUSH);
    };
    Ok(status_code(status))
}

fn stream_reader<R, W>(mut reader: R, writer: W, mut processor: Processor) -> Result<()>
where
    R: Read,
    W: Write,
{
    let mut writer = BufWriter::with_capacity(OUTPUT_BUFFER_SIZE, writer);
    let mut input = [0_u8; READ_BUFFER_SIZE];
    let mut output = Vec::with_capacity(READ_BUFFER_SIZE + 1024);

    loop {
        let read = match reader.read(&mut input) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error).context("reading output failed"),
        };
        output.clear();
        processor.push(&input[..read], &mut output)?;
        writer.write_all(&output).context("writing output failed")?;
        // A read is at most 32 KiB, so this bounds display latency without
        // turning individual lines into individual write syscalls.
        writer.flush().context("flushing output failed")?;
    }

    output.clear();
    processor.flush(&mut output)?;
    writer.write_all(&output).context("writing output failed")?;
    writer.flush().context("flushing output failed")?;
    Ok(())
}

enum ReadEvent {
    Data(Vec<u8>),
    Eof,
    Error(io::Error),
}

fn spawn_bounded_reader<R>(
    mut reader: R,
    thread_name: &str,
    pty_eio_is_eof: bool,
) -> Result<(mpsc::Receiver<ReadEvent>, mpsc::SyncSender<Vec<u8>>)>
where
    R: Read + Send + 'static,
{
    let (event_sender, event_receiver) = mpsc::sync_channel(PTY_QUEUE_DEPTH);
    let (recycle_sender, recycle_receiver) = mpsc::sync_channel(PTY_QUEUE_DEPTH);
    thread::Builder::new()
        .name(thread_name.to_owned())
        .spawn(move || {
            loop {
                let mut buffer = recycle_receiver
                    .try_recv()
                    .unwrap_or_else(|_| Vec::with_capacity(READ_BUFFER_SIZE));
                buffer.resize(READ_BUFFER_SIZE, 0);
                match reader.read(&mut buffer) {
                    Ok(0) => {
                        let _ = event_sender.send(ReadEvent::Eof);
                        break;
                    }
                    Ok(read) => {
                        buffer.truncate(read);
                        if event_sender.send(ReadEvent::Data(buffer)).is_err() {
                            break;
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    // Linux and macOS PTY masters may report EIO after the
                    // slave closes instead of returning a zero-length read.
                    Err(error) if pty_eio_is_eof && error.raw_os_error() == Some(5) => {
                        let _ = event_sender.send(ReadEvent::Eof);
                        break;
                    }
                    Err(error) => {
                        let _ = event_sender.send(ReadEvent::Error(error));
                        break;
                    }
                }
            }
        })
        .with_context(|| format!("unable to start the {thread_name} worker"))?;
    Ok((event_receiver, recycle_sender))
}

fn run_interactive(program: &OsStr, args: &[OsString], mut processor: Processor) -> Result<u32> {
    let initial_size = current_terminal_size();
    let mut session = PtySession::spawn(program, args, initial_size)
        .with_context(|| format!("unable to start {program:?} in a PTY"))?;
    let reader = session
        .try_clone_reader()
        .context("unable to open the PTY output stream")?;
    let mut writer = session
        .take_writer()
        .context("unable to open the PTY input stream")?;

    let resize_pending = Arc::new(AtomicBool::new(false));
    let termination_signal = Arc::new(AtomicUsize::new(0));
    register_terminal_flags(Arc::clone(&resize_pending), Arc::clone(&termination_signal))?;

    // Signal handlers must be live before raw mode is entered: process-default
    // termination would otherwise bypass Drop in the tiny setup window. This
    // guard restores terminal state on every ordinary return and error path.
    let _raw_mode = RawModeGuard::enable().context("unable to enable terminal raw mode")?;

    let (event_receiver, recycle_sender) = spawn_bounded_reader(reader, "shue-pty-output", true)?;

    thread::Builder::new()
        .name("shue-pty-input".into())
        .spawn(move || {
            let stdin = io::stdin();
            let mut stdin = stdin.lock();
            let mut buffer = [0_u8; 16 * 1024];
            loop {
                let read = match stdin.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(read) => read,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                };
                if writer.write_all(&buffer[..read]).is_err() || writer.flush().is_err() {
                    break;
                }
            }
        })
        .context("unable to start the PTY input worker")?;

    let stdout = io::stdout();
    let mut stdout = BufWriter::with_capacity(OUTPUT_BUFFER_SIZE, stdout.lock());
    let mut output = Vec::with_capacity(READ_BUFFER_SIZE + 1024);
    let mut dirty = false;
    let mut last_writer_flush = Instant::now();

    let terminated_by_signal = loop {
        let signal = termination_signal.load(Ordering::Relaxed);
        if signal != 0 {
            break Some(signal as u32);
        }
        if resize_pending.swap(false, Ordering::Relaxed) {
            session
                .resize(current_terminal_size())
                .context("unable to resize the child PTY")?;
        }

        match event_receiver.recv_timeout(PROMPT_IDLE_FLUSH) {
            Ok(ReadEvent::Data(input)) => {
                output.clear();
                processor.push(&input, &mut output)?;
                stdout
                    .write_all(&output)
                    .context("writing PTY output failed")?;
                dirty = true;
                let has_newline = input.contains(&b'\n');
                // Return the allocation to the reader without blocking this
                // loop. The event queue is the bounded backpressure mechanism.
                let _ = recycle_sender.try_send(input);
                if has_newline || last_writer_flush.elapsed() >= DISPLAY_FLUSH_INTERVAL {
                    stdout.flush().context("flushing PTY output failed")?;
                    last_writer_flush = Instant::now();
                }
            }
            Ok(ReadEvent::Eof) => break None,
            Ok(ReadEvent::Error(error)) => {
                return Err(error).context("reading PTY output failed");
            }
            Err(mpsc::RecvTimeoutError::Timeout) if dirty => {
                output.clear();
                processor.flush(&mut output)?;
                stdout
                    .write_all(&output)
                    .context("writing PTY output failed")?;
                stdout.flush().context("flushing PTY prompt failed")?;
                last_writer_flush = Instant::now();
                dirty = false;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break None,
        }
    };

    output.clear();
    processor.flush(&mut output)?;
    stdout
        .write_all(&output)
        .context("writing PTY output failed")?;
    stdout.flush().context("flushing PTY output failed")?;
    drop(stdout);

    if let Some(signal) = terminated_by_signal {
        session
            .terminate()
            .context("unable to terminate the PTY child after receiving a signal")?;
        let _ = session
            .wait()
            .context("unable to reap the terminated PTY child")?;
        Ok(128 + signal)
    } else {
        session.wait().context("unable to wait for the PTY child")
    }
}

fn current_terminal_size() -> TerminalSize {
    TerminalSize::current().unwrap_or_default()
}

#[cfg(unix)]
fn register_terminal_flags(resize: Arc<AtomicBool>, termination: Arc<AtomicUsize>) -> Result<()> {
    use signal_hook::consts::SIGWINCH;

    signal_hook::flag::register(SIGWINCH, resize)
        .context("unable to install the terminal resize handler")?;
    register_termination_flags(termination)
}

#[cfg(unix)]
fn register_termination_flags(termination: Arc<AtomicUsize>) -> Result<()> {
    use signal_hook::consts::{SIGHUP, SIGINT, SIGQUIT, SIGTERM};

    for signal in [SIGHUP, SIGINT, SIGQUIT, SIGTERM] {
        signal_hook::flag::register_usize(signal, Arc::clone(&termination), signal as usize)
            .context("unable to install a termination handler")?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn register_terminal_flags(_resize: Arc<AtomicBool>, _termination: Arc<AtomicUsize>) -> Result<()> {
    Err(anyhow::anyhow!(
        "interactive PTY mode is supported only on Unix"
    ))
}

#[cfg(not(unix))]
fn register_termination_flags(_termination: Arc<AtomicUsize>) -> Result<()> {
    Err(anyhow::anyhow!(
        "child signal forwarding is supported only on Unix"
    ))
}

#[cfg(unix)]
fn status_code(status: ExitStatus) -> u32 {
    use std::os::unix::process::ExitStatusExt;

    status
        .code()
        .map(|code| code as u32)
        .or_else(|| status.signal().map(|signal| 128 + signal as u32))
        .unwrap_or(1)
}

#[cfg(not(unix))]
fn status_code(status: ExitStatus) -> u32 {
    status.code().map(|code| code as u32).unwrap_or(1)
}

fn is_broken_pipe(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<io::Error>()
            .is_some_and(|error| error.kind() == io::ErrorKind::BrokenPipe)
    })
}

fn concise_error(error: &anyhow::Error) -> String {
    let mut messages: Vec<String> = Vec::new();
    for cause in error.chain() {
        let message = cause.to_string();
        // Some typed errors include their source in Display while also exposing
        // it through Error::source. Avoid printing that suffix twice.
        if messages
            .last()
            .is_some_and(|outer| outer.ends_with(&format!(": {message}")))
        {
            continue;
        }
        messages.push(message);
    }
    messages.join(": ")
}
