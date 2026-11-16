//! Detached installation worker. Credentials pass only over an anonymous pipe.
use crate::{config::Config, install};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    io::{self, Read, Write},
    os::unix::{fs::PermissionsExt, process::CommandExt},
    path::Path,
    process::{Command, Stdio},
    thread,
};

const DIRECTORY: &str = "/run/stagehand";
const LOCK: &str = "/run/stagehand/active.lock";
pub const STATUS_PATH: &str = "/run/stagehand/status.json";
const MAX_CONFIG_BYTES: u64 = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunState {
    Running,
    Finished,
    Failed,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunStatus {
    pub pid: u32,
    pub state: RunState,
    /// Current 1-indexed phase; zero means the worker is initializing.
    pub phase: usize,
    /// Completed phases, never an estimated duration percentage.
    pub completed: usize,
    pub detail: String,
    pub error: Option<String>,
}
impl RunStatus {
    fn starting(pid: u32) -> Self {
        Self {
            pid,
            state: RunState::Running,
            phase: 0,
            completed: 0,
            detail: "Preparing installer worker".into(),
            error: None,
        }
    }
    fn entering(&mut self, phase: usize, detail: &str) {
        self.phase = phase;
        self.completed = phase.saturating_sub(1).min(install::STEP_COUNT);
        self.detail = detail.to_string();
    }
    fn finish(&mut self) {
        self.state = RunState::Finished;
        self.phase = install::STEP_COUNT;
        self.completed = install::STEP_COUNT;
        self.detail = "Gentoo installation completed".into();
        self.error = None;
    }
    fn fail(&mut self, error: String) {
        self.state = RunState::Failed;
        self.error = Some(error);
    }
}
fn ensure_directory() -> Result<()> {
    fs::create_dir_all(DIRECTORY).context("Create Stagehand runtime directory")?;
    fs::set_permissions(DIRECTORY, fs::Permissions::from_mode(0o700))?;
    Ok(())
}
fn publish(status: &RunStatus) -> Result<()> {
    ensure_directory()?;
    // Rename is atomic: readers never observe a partially written JSON record.
    let temporary = format!("{DIRECTORY}/.status-{}.tmp", status.pid);
    let mut file = File::create(&temporary)?;
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    serde_json::to_writer(&mut file, status)?;
    file.flush()?;
    fs::rename(&temporary, STATUS_PATH)?;
    Ok(())
}
pub fn read_status() -> Result<Option<RunStatus>> {
    match fs::read(STATUS_PATH) {
        Ok(data) => Ok(Some(
            serde_json::from_slice(&data).context("Invalid Stagehand status JSON")?,
        )),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Linux-only PID check avoids mistaking a reused PID for our worker.
pub fn worker_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    fs::read(format!("/proc/{pid}/cmdline"))
        .is_ok_and(|data| data.split(|b| *b == 0).any(|arg| arg == b"--worker"))
}

/// Launch independent of the interactive terminal; never put passwords in argv,
/// environment, logs or files. The TUI can exit while this process runs.
pub fn spawn(config: &Config) -> Result<u32> {
    config.validate().map_err(anyhow::Error::msg)?;
    if let Some(status) = read_status()? {
        if status.state == RunState::Running && worker_alive(status.pid) {
            bail!("An installation is already running (PID {}).", status.pid);
        }
    }
    if Path::new(LOCK).exists() {
        bail!("Stagehand worker lock exists at {LOCK}. Inspect possible unfinished disk operations before manual recovery.");
    }
    // Discard the previous finished/failed snapshot only after a new
    // installation was deliberately authorized and no worker lock exists.
    match fs::remove_file(STATUS_PATH) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    let mut json = serde_json::to_vec(config)?;
    let mut cmd = Command::new(std::env::current_exe()?);
    cmd.arg("--worker")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: setsid is async-signal-safe and performs no Rust allocation in
    // the post-fork child before exec. The worker has no controlling terminal.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    let mut child = cmd.spawn().context("Launch detached installer worker")?;
    let pid = child.id();
    let transfer = child
        .stdin
        .take()
        .context("Worker stdin unavailable")?
        .write_all(&json);
    json.fill(0);
    transfer.context("Transfer installation settings to worker")?;
    // Reap the worker while TUI remains open, preventing a zombie child.
    thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(pid)
}

/// Internal mode, invoked from the same executable with --worker.
pub fn run() -> Result<()> {
    let mut bytes = Vec::new();
    io::stdin()
        .take(MAX_CONFIG_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_CONFIG_BYTES {
        bytes.fill(0);
        bail!("Installer configuration exceeds size limit");
    }
    let decoded = serde_json::from_slice::<Config>(&bytes);
    bytes.fill(0);
    let config = decoded.context("Read installer config from anonymous stdin pipe")?;
    let _lock = WorkerLock::acquire()?;
    let mut status = RunStatus::starting(std::process::id());
    publish(&status)?;
    let outcome = install::run(&config, &mut |phase, name| {
        status.entering(phase, name);
        publish(&status)
    });
    match outcome {
        Ok(()) => status.finish(),
        Err(e) => status.fail(format!("{e:#}")),
    }
    let failed = status.state == RunState::Failed;
    publish(&status)?;
    if failed {
        bail!(
            "Installation failed. See {} and {}",
            install::LOG,
            STATUS_PATH
        );
    }
    Ok(())
}

/// Atomic lock acquisition prevents concurrent installers. SIGKILL leaves
/// the lock for explicit manual investigation, never silent reinstallation.
struct WorkerLock;
impl WorkerLock {
    fn acquire() -> Result<Self> {
        ensure_directory()?;
        fs::create_dir(LOCK).with_context(|| {
            format!("Stagehand worker lock {LOCK} is occupied; refusing concurrent install")
        })?;
        Ok(Self)
    }
}
impl Drop for WorkerLock {
    fn drop(&mut self) {
        let _ = fs::remove_dir(LOCK);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn progress_is_only_completed_steps() {
        let mut s = RunStatus::starting(42);
        s.entering(9, "Compiling kernel");
        assert_eq!(s.completed, 8);
        assert_eq!(s.phase, 9);
        s.finish();
        assert_eq!(s.completed, install::STEP_COUNT);
        assert_eq!(s.state, RunState::Finished);
    }
    #[test]
    fn status_has_no_credentials() {
        let mut s = RunStatus::starting(42);
        s.entering(1, "Preflight checks");
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("Preflight checks"));
        assert!(!json.contains("user_password"));
        assert!(!json.contains("root_password"));
        let restored: RunStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.pid, 42);
    }
    #[test]
    fn failure_retains_phase() {
        let mut s = RunStatus::starting(5);
        s.entering(4, "Mount filesystems");
        s.fail("mount failed".into());
        assert_eq!(s.completed, 3);
        assert_eq!(s.state, RunState::Failed);
    }
}
