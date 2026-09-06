//! Plugin runner — spawns Lua-declared subprocesses and bridges stdio.
//!
//! The runner executes `command[0]` directly with `command[1..]` as argv.
//! It does not invoke a shell, infer installation layout, or add per-plugin
//! cwd/environment policy. Children inherit the engine cwd and environment.

use std::process::Stdio;

use tokio::process::Command;
use tokio::sync::oneshot;

use crate::ncp::error::BrokerError;
use crate::ncp::spawn::{PluginSpec, TerminalMode};
use crate::ncp::transport::{stdio_transport, ExitOutcome, Transport};
#[cfg(unix)]
use std::os::fd::AsRawFd as _;

/// Spawn the subprocess explicitly declared by `spec`.
///
/// The caller filters virtual specs (`spec.command().is_none()`) before
/// calling. The runner rejects them rather than guessing what to spawn.
pub fn spawn_plugin(spec: &PluginSpec) -> Result<Transport, BrokerError> {
    let command = spec.command().ok_or_else(|| BrokerError::Spawn {
        name: spec.name.as_str().to_owned(),
        command: Vec::new(),
        source: std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "spec has no command (virtual plugin must not be subprocess-spawned)",
        ),
    })?;

    let (binary, args) = command.split_first().ok_or_else(|| BrokerError::Spawn {
        name: spec.name.as_str().to_owned(),
        command: command.to_vec(),
        source: std::io::Error::new(std::io::ErrorKind::InvalidInput, "empty command array"),
    })?;

    let mut cmd = Command::new(binary);
    cmd.args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0);
    let mut foreground_terminal =
        configure_terminal_foreground(&mut cmd, spec.terminal, &spec.name).map_err(|source| {
            BrokerError::Spawn {
                name: spec.name.as_str().to_owned(),
                command: command.to_vec(),
                source,
            }
        })?;

    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(source) => {
            restore_terminal_foreground(&mut foreground_terminal, &spec.name);
            return Err(BrokerError::Spawn {
                name: spec.name.as_str().to_owned(),
                command: command.to_vec(),
                source,
            });
        }
    };

    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| io_err("child stdin missing"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io_err("child stdout missing"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| io_err("child stderr missing"))?;

    let process_group = child.id();
    let plugin_name = spec.name.clone();
    let (terminate, terminate_rx) = oneshot::channel();
    let exit = Box::pin(async move {
        let waited = tokio::select! {
            status = child.wait() => status,
            _ = terminate_rx => {
                terminate_process_tree(&mut child, process_group);
                child.wait().await
            }
        };
        terminate_descendants(process_group);
        restore_terminal_foreground(&mut foreground_terminal, &plugin_name);
        match waited {
            Ok(status) if status.success() => ExitOutcome::CleanExit,
            Ok(status) => exit_outcome(status),
            Err(error) => ExitOutcome::Unknown {
                reason: error.to_string(),
            },
        }
    });

    Ok(stdio_transport(stdin, stdout, stderr, terminate, exit))
}

#[cfg(unix)]
struct ForegroundTerminal {
    tty: std::fs::File,
    previous_group: libc::pid_t,
    plugin_name: nefor_protocol::PluginName,
    restoration_attempted: bool,
}

#[cfg(unix)]
impl ForegroundTerminal {
    fn restore(&mut self) -> std::io::Result<()> {
        self.restoration_attempted = true;
        set_terminal_foreground(self.tty.as_raw_fd(), self.previous_group)
    }
}

#[cfg(unix)]
impl Drop for ForegroundTerminal {
    fn drop(&mut self) {
        if self.restoration_attempted {
            return;
        }
        if let Err(error) = self.restore() {
            tracing::error!(
                plugin = %self.plugin_name,
                %error,
                previous_group = self.previous_group,
                "failed to restore foreground terminal authority"
            );
        }
    }
}

#[cfg(not(unix))]
type ForegroundTerminal = ();

#[cfg(unix)]
fn configure_terminal_foreground(
    command: &mut Command,
    mode: TerminalMode,
    plugin_name: &nefor_protocol::PluginName,
) -> std::io::Result<Option<ForegroundTerminal>> {
    if mode == TerminalMode::Detached {
        return Ok(None);
    }

    let tty = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")?;
    let tty_fd = tty.as_raw_fd();
    let previous_group = unsafe { libc::tcgetpgrp(tty_fd) };
    if previous_group == -1 {
        return Err(std::io::Error::last_os_error());
    }
    unsafe {
        command.pre_exec(move || set_terminal_foreground(tty_fd, libc::getpgrp()));
    }
    Ok(Some(ForegroundTerminal {
        tty,
        previous_group,
        plugin_name: plugin_name.clone(),
        restoration_attempted: false,
    }))
}

#[cfg(not(unix))]
fn configure_terminal_foreground(
    _command: &mut Command,
    _mode: TerminalMode,
    _plugin_name: &nefor_protocol::PluginName,
) -> std::io::Result<Option<ForegroundTerminal>> {
    Ok(None)
}

#[cfg(unix)]
fn restore_terminal_foreground(
    terminal: &mut Option<ForegroundTerminal>,
    plugin_name: &nefor_protocol::PluginName,
) {
    let Some(mut terminal) = terminal.take() else {
        return;
    };
    if let Err(error) = terminal.restore() {
        tracing::error!(
            plugin = %plugin_name,
            %error,
            previous_group = terminal.previous_group,
            "failed to restore foreground terminal authority"
        );
    }
}

#[cfg(not(unix))]
fn restore_terminal_foreground(
    _terminal: &mut Option<ForegroundTerminal>,
    _plugin_name: &nefor_protocol::PluginName,
) {
}

#[cfg(unix)]
fn set_terminal_foreground(fd: std::os::fd::RawFd, group: libc::pid_t) -> std::io::Result<()> {
    unsafe {
        let mut blocked = std::mem::zeroed();
        if libc::sigemptyset(&mut blocked) == -1
            || libc::sigaddset(&mut blocked, libc::SIGTTOU) == -1
        {
            return Err(std::io::Error::last_os_error());
        }
        let mut previous = std::mem::zeroed();
        if libc::sigprocmask(libc::SIG_BLOCK, &blocked, &mut previous) == -1 {
            return Err(std::io::Error::last_os_error());
        }
        let result = libc::tcsetpgrp(fd, group);
        let terminal_error = (result == -1).then(std::io::Error::last_os_error);
        if libc::sigprocmask(libc::SIG_SETMASK, &previous, std::ptr::null_mut()) == -1 {
            return Err(std::io::Error::last_os_error());
        }
        match terminal_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

fn terminate_process_tree(child: &mut tokio::process::Child, process_group: Option<u32>) {
    if terminate_descendants(process_group) {
        return;
    }
    let _ = child.start_kill();
}

fn terminate_descendants(process_group: Option<u32>) -> bool {
    #[cfg(unix)]
    if let Some(process_group) = process_group {
        // Each engine-owned plugin is the leader of its own group, so this
        // includes helper descendants the plugin did not reap itself.
        unsafe {
            libc::killpg(process_group as libc::pid_t, libc::SIGKILL);
        }
        return true;
    }
    #[cfg(not(unix))]
    let _ = process_group;
    false
}

fn exit_outcome(status: std::process::ExitStatus) -> ExitOutcome {
    if let Some(code) = status.code() {
        return ExitOutcome::ExitCode(code);
    }

    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt as _;
        if let Some(signal) = status.signal() {
            return ExitOutcome::Signal(signal);
        }
    }

    ExitOutcome::Crash
}

fn io_err(msg: &str) -> BrokerError {
    BrokerError::Io(std::io::Error::other(msg))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nefor_protocol::PluginName;

    #[tokio::test]
    async fn spawn_plugin_executes_declared_command_without_a_layout_root() {
        let spec = PluginSpec {
            name: PluginName::new("echo-plugin").expect("valid"),
            terminal: TerminalMode::Detached,
            kind: crate::ncp::spawn::PluginKind::Command(vec!["echo".into()]),
        };
        assert!(spawn_plugin(&spec).is_ok());
    }

    #[test]
    fn spawn_plugin_rejects_virtual_spec() {
        let spec = PluginSpec {
            name: PluginName::new("virtual").expect("valid"),
            terminal: TerminalMode::Detached,
            kind: crate::ncp::spawn::PluginKind::Cli,
        };
        match spawn_plugin(&spec) {
            Err(BrokerError::Spawn { source, .. }) => {
                assert_eq!(source.kind(), std::io::ErrorKind::InvalidInput);
            }
            Err(other) => panic!("expected Spawn err for virtual spec, got {other:?}"),
            Ok(_) => panic!("expected error for virtual spec"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn foreground_terminal_is_restored_after_process_completion_and_spawn_failure() {
        for case in ["natural", "forced", "spawn-failure"] {
            run_terminal_process_case(case);
        }
    }

    #[cfg(unix)]
    fn run_terminal_process_case(case: &str) {
        use std::fs::File;
        use std::os::fd::{AsRawFd as _, FromRawFd as _};
        use std::os::unix::process::CommandExt as _;
        use std::time::{Duration, Instant};

        let mut master_fd = -1;
        let mut slave_fd = -1;
        let opened = unsafe {
            libc::openpty(
                &mut master_fd,
                &mut slave_fd,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(
            opened,
            0,
            "openpty failed: {}",
            std::io::Error::last_os_error()
        );
        let master = unsafe { File::from_raw_fd(master_fd) };
        let slave = unsafe { File::from_raw_fd(slave_fd) };
        let scratch = tempfile::tempdir().expect("scratch directory");
        let pid_path = scratch.path().join("plugin.pid");
        let release_path = scratch.path().join("release");

        let mut command = std::process::Command::new(std::env::current_exe().expect("test binary"));
        command
            .arg("--exact")
            .arg("ncp::runner::tests::terminal_process_helper")
            .arg("--nocapture")
            .env("NEFOR_TERMINAL_TEST_CASE", case)
            .env("NEFOR_TERMINAL_TEST_PID", &pid_path)
            .env("NEFOR_TERMINAL_TEST_RELEASE", &release_path);
        let controlling_tty = slave.as_raw_fd();
        unsafe {
            command.pre_exec(move || {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::ioctl(controlling_tty, libc::TIOCSCTTY as _, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                set_terminal_foreground(controlling_tty, libc::getpgrp())
            });
        }
        let mut helper = command.spawn().expect("spawn terminal test helper");
        drop(slave);

        if case != "spawn-failure" {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !pid_path.exists() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            let plugin_group: libc::pid_t = std::fs::read_to_string(&pid_path)
                .expect("plugin pid marker")
                .trim()
                .parse()
                .expect("plugin pid");
            while unsafe { libc::tcgetpgrp(master.as_raw_fd()) } != plugin_group
                && Instant::now() < deadline
            {
                std::thread::sleep(Duration::from_millis(10));
            }
            assert_eq!(
                unsafe { libc::tcgetpgrp(master.as_raw_fd()) },
                plugin_group,
                "plugin process group never received foreground authority ({case})"
            );
            File::create(&release_path).expect("release plugin/helper");
        }

        let status = helper.wait().expect("wait for terminal test helper");
        assert!(
            status.success(),
            "terminal helper failed for {case}: {status}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn terminal_process_helper() {
        use std::os::fd::AsRawFd as _;
        use std::time::{Duration, Instant};

        let Ok(case) = std::env::var("NEFOR_TERMINAL_TEST_CASE") else {
            return;
        };
        let pid_path = std::env::var("NEFOR_TERMINAL_TEST_PID").expect("pid path");
        let release_path = std::env::var("NEFOR_TERMINAL_TEST_RELEASE").expect("release path");
        let own_group = unsafe { libc::getpgrp() };
        let tty = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/tty")
            .expect("helper controlling tty");
        assert_eq!(unsafe { libc::tcgetpgrp(tty.as_raw_fd()) }, own_group);

        let command = match case.as_str() {
            "spawn-failure" => vec!["/definitely/missing/nefor-terminal-test-plugin".into()],
            "natural" => vec![
                "sh".into(),
                "-c".into(),
                format!(
                    "printf '%s' \"$$\" > '{}'; while [ ! -e '{}' ]; do sleep 0.01; done",
                    pid_path, release_path
                ),
            ],
            "forced" => vec![
                "sh".into(),
                "-c".into(),
                format!(
                    "printf '%s' \"$$\" > '{}'; while :; do sleep 0.01; done",
                    pid_path
                ),
            ],
            other => panic!("unknown terminal test case: {other}"),
        };
        let spec = PluginSpec {
            name: PluginName::new("terminal-test-plugin").expect("valid"),
            terminal: TerminalMode::Foreground,
            kind: crate::ncp::spawn::PluginKind::Command(command),
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let _runtime_guard = runtime.enter();

        if case == "spawn-failure" {
            assert!(matches!(
                spawn_plugin(&spec),
                Err(BrokerError::Spawn { .. })
            ));
        } else {
            let mut transport = spawn_plugin(&spec).expect("spawn foreground plugin");
            if case == "forced" {
                let deadline = Instant::now() + Duration::from_secs(5);
                while !std::path::Path::new(&release_path).exists() && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(10));
                }
                transport
                    .terminate
                    .take()
                    .expect("termination handle")
                    .send(())
                    .expect("request forced teardown");
            }
            let outcome = runtime.block_on(transport.exit.take().expect("exit watcher"));
            match case.as_str() {
                "natural" => assert_eq!(outcome, ExitOutcome::CleanExit),
                "forced" => assert_eq!(outcome, ExitOutcome::Signal(libc::SIGKILL)),
                other => panic!("unknown terminal test case: {other}"),
            }
        }

        assert_eq!(
            unsafe { libc::tcgetpgrp(tty.as_raw_fd()) },
            own_group,
            "engine process group was not restored after {case}"
        );
    }
}
