//! Local shell sessions backed by portable-pty (ConPTY on Windows, openpty
//! on Unix — the same crate WezTerm uses).

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use portable_pty::{native_pty_system, Child, ChildKiller, CommandBuilder, MasterPty, PtySize};

use super::shell_integration::{self, resolve_reported_cwd, CwdSource};
use super::proc_cwd;
use super::{EventSink, LocalSpec, SessionError, SessionMap};

/// Shared PTY writer handle. Cloned out of the session map so the (blocking)
/// write happens *after* the map lock is released — see `SessionManager::write`.
pub(crate) type LocalWriter = Arc<Mutex<Box<dyn Write + Send>>>;

/// The output half of a spawned shell, handed back to the caller instead of
/// being pumped immediately. `SessionManager` registers the session in its map
/// and only then calls [`LocalReader::start`].
///
/// The ordering matters: the reader is what *removes* the session from the map
/// (and emits `exit`) when the shell ends. If it ran while the session was
/// still on its way into the map — which a shell that exits instantly, say a
/// mistyped executable, makes easy to hit — the removal would happen before
/// the insert, and the entry would then sit in the map forever: never reaped,
/// never killable (the frontend already tore its thread down on `exit`), and
/// holding the PTY, the child handle and the shell-integration temp dir.
pub(crate) struct LocalReader {
    reader: Box<dyn Read + Send>,
    child: Box<dyn Child + Send + Sync>,
    /// Where the shell's cwd can be read from when OSC 7 can't be trusted to
    /// arrive (Windows ConPTY), and the child's pid for the process source.
    cwd_source: Option<CwdSource>,
    pid: Option<u32>,
}

/// How often the cwd source is checked. A prompt-to-prompt change is human
/// speed; this keeps the task pane feeling live without busy-polling.
const CWD_POLL_INTERVAL: Duration = Duration::from_millis(300);

/// Turns a [`CwdSource`] into "the cwd, if it changed since last asked".
struct CwdPoller {
    source: CwdSource,
    pid: Option<u32>,
    /// `(len, mtime)` of the cwd file at the last read — the file is only
    /// re-read when this moves.
    stamp: Option<(u64, SystemTime)>,
    last_emitted: Option<String>,
}

impl CwdPoller {
    fn new(source: CwdSource, pid: Option<u32>) -> Self {
        Self { source, pid, stamp: None, last_emitted: None }
    }

    /// The resolved cwd when it differs from the last one returned; `None` for
    /// "nothing new", including an empty or unreadable source.
    fn poll(&mut self) -> Option<String> {
        let raw = match &self.source {
            CwdSource::File(path) => {
                let meta = std::fs::metadata(path).ok()?;
                let stamp = (meta.len(), meta.modified().ok()?);
                if self.stamp == Some(stamp) {
                    return None;
                }
                // Only remember the stamp once the read worked, so a
                // transient failure is retried on the next tick.
                let raw = std::fs::read_to_string(path).ok()?;
                self.stamp = Some(stamp);
                raw
            }
            CwdSource::Process => proc_cwd::process_cwd(self.pid?)?,
        };
        let cwd = resolve_reported_cwd(&raw, cfg!(windows))?;
        if self.last_emitted.as_deref() == Some(cwd.as_str()) {
            return None;
        }
        self.last_emitted = Some(cwd.clone());
        Some(cwd)
    }
}

impl LocalReader {
    /// Pumps PTY output to the sink until EOF, then reaps the child, drops the
    /// session from the map and announces the exit. Call only after the
    /// session has been inserted into `sessions`.
    pub fn start(self, id: String, sink: Arc<dyn EventSink>, sessions: SessionMap) {
        let LocalReader { mut reader, mut child, cwd_source, pid } = self;
        // Set by the reader thread before it announces the exit, so the poller
        // can never emit a `cwd` for a session the frontend already dropped.
        let ended = Arc::new(AtomicBool::new(false));
        if let Some(source) = cwd_source {
            let ended = ended.clone();
            let sink = sink.clone();
            let id = id.clone();
            std::thread::spawn(move || {
                let mut poller = CwdPoller::new(source, pid);
                while !ended.load(Ordering::Acquire) {
                    if let Some(cwd) = poller.poll() {
                        if ended.load(Ordering::Acquire) {
                            break;
                        }
                        sink.cwd(&id, &cwd);
                    }
                    std::thread::sleep(CWD_POLL_INTERVAL);
                }
            });
        }
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => sink.data(&id, &buf[..n]),
                }
            }
            let code = child.wait().map(|s| s.exit_code() as i32).unwrap_or(-1);
            sessions.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
            ended.store(true, Ordering::Release);
            sink.exit(&id, code);
        });
    }
}

pub(crate) struct LocalSession {
    master: Mutex<Box<dyn MasterPty + Send>>,
    writer: LocalWriter,
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    // Keeps the generated shell-integration rcfile/ZDOTDIR and the cwd file
    // alive for the session's lifetime — the shell only reads it at startup, but holding it
    // for the whole session (rather than deleting right after spawn) avoids
    // any race with a slow-starting shell.
    _integration_guard: Option<tempfile::TempDir>,
}

impl LocalSession {
    /// Opens a PTY and starts the shell. Returns the session, its display
    /// title, and the not-yet-running [`LocalReader`] — see that type for why
    /// output pumping only begins once the caller has registered the session.
    pub fn spawn(
        spec: &LocalSpec,
        cols: u16,
        rows: u16,
        shell_integration_enabled: bool,
    ) -> Result<(Self, String, LocalReader), SessionError> {
        // Before portable-pty's first ConPTY lookup (it loads `conpty.dll` by
        // name); a no-op off Windows and after the first call.
        super::conpty_sideload::ensure_loaded();
        let shell = if spec.command.is_empty() {
            default_shell()
        } else {
            spec.command.clone()
        };

        let pty = native_pty_system();
        let pair = pty
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| SessionError::Pty(e.to_string()))?;

        // Best-effort: a recognized shell (bash/zsh/fish/nu/PowerShell/cmd/wsl)
        // gets OSC 133 hooks and a cwd side channel spliced into its launch, so
        // the frontend's command-block UI and task pane work without the user
        // editing any rc file themselves.
        let integration = shell_integration_enabled
            .then(|| shell_integration::build(&shell, &spec.args))
            .flatten();

        let mut cmd = CommandBuilder::new(&shell);
        if let Some(integ) = &integration {
            for arg in &integ.prefix_args {
                cmd.arg(arg);
            }
        }
        // An integration may replace the profile's args (bash drops its
        // login flags and emulates them in the rcfile instead).
        let profile_args = integration
            .as_ref()
            .and_then(|i| i.replace_args.as_ref())
            .unwrap_or(&spec.args);
        for arg in profile_args {
            cmd.arg(arg);
        }
        if let Some(integ) = &integration {
            for arg in &integ.suffix_args {
                cmd.arg(arg);
            }
            for (k, v) in &integ.env {
                cmd.env(k, v);
            }
        }
        cmd.env("TERM", "xterm-256color");
        cmd.env("TERM_PROGRAM", "senju-term");
        let cwd = expand_home(&spec.cwd).or_else(dirs::home_dir);
        if let Some(dir) = cwd {
            cmd.cwd(dir);
        }

        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| SessionError::Pty(e.to_string()))?;
        drop(pair.slave);

        let pid = child.process_id();
        let killer = child.clone_killer();
        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| SessionError::Pty(e.to_string()))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| SessionError::Pty(e.to_string()))?;

        let title = if spec.title.is_empty() {
            shell.rsplit(['/', '\\']).next().unwrap_or(&shell).to_string()
        } else {
            spec.title.clone()
        };
        let cwd_source = integration.as_ref().and_then(|i| i.cwd_source.clone());
        Ok((
            Self {
                master: Mutex::new(pair.master),
                writer: Arc::new(Mutex::new(writer)),
                killer: Mutex::new(killer),
                _integration_guard: integration.and_then(|i| i.guard),
            },
            title,
            LocalReader { reader, child, cwd_source, pid },
        ))
    }

    /// A cloneable handle to the PTY writer, so the caller can release the
    /// session-map lock before writing.
    pub fn writer_handle(&self) -> LocalWriter {
        self.writer.clone()
    }

    /// Blocking PTY write. MUST be called off the session-map lock (and off an
    /// async worker, via `spawn_blocking`): a full PTY buffer — e.g. a stopped
    /// or Ctrl-S'd child — can block this until the buffer drains, which would
    /// otherwise deadlock every other session operation.
    pub fn write_blocking(writer: &LocalWriter, data: &[u8]) -> Result<(), SessionError> {
        writer
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .write_all(data)
            .map_err(|e| SessionError::Pty(e.to_string()))
    }

    pub fn resize(&self, cols: u16, rows: u16) -> Result<(), SessionError> {
        self.master
            .lock()
            .unwrap()
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| SessionError::Pty(e.to_string()))
    }

    pub fn kill(self) {
        let _ = self.killer.lock().unwrap().kill();
        // Dropping `master`/`writer` closes the PTY, which unblocks the
        // reader thread on platforms where kill alone doesn't.
    }
}

fn default_shell() -> String {
    if cfg!(windows) {
        windows_default_shell(std::env::var_os("PATH"))
    } else {
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into())
    }
}

/// PowerShell 7 (`pwsh.exe`) when it is on `PATH`, else the Windows
/// PowerShell 5.1 that ships with every Windows.
///
/// Deliberately not `%COMSPEC%`: that is cmd.exe on practically every machine,
/// and cmd.exe has no prompt hook to inject shell integration into — so the
/// default thread got no command blocks and the task pane could never learn
/// the working directory. Windows Terminal made the same call and opens
/// PowerShell by default. cmd.exe is still one click away as its own profile.
fn windows_default_shell(path: Option<std::ffi::OsString>) -> String {
    let has_pwsh = path.is_some_and(|paths| {
        std::env::split_paths(&paths).any(|dir| dir.join("pwsh.exe").is_file())
    });
    if has_pwsh { "pwsh.exe" } else { "powershell.exe" }.into()
}

/// Expands a leading `~` to the home directory. Returns `None` for an empty
/// path so the caller can fall back to the home directory.
fn expand_home(path: &str) -> Option<std::path::PathBuf> {
    if path.is_empty() {
        return None;
    }
    if let Some(rest) = path.strip_prefix('~') {
        if let Some(home) = dirs::home_dir() {
            let rest = rest.strip_prefix(['/', '\\']).unwrap_or(rest);
            return Some(home.join(rest));
        }
    }
    Some(std::path::PathBuf::from(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn poller_reads_the_file_only_when_it_changes_and_dedups() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("cwd");
        let mut p = CwdPoller::new(CwdSource::File(file.clone()), None);
        assert_eq!(p.poll(), None, "missing file emits nothing");

        std::fs::write(&file, "").unwrap();
        assert_eq!(p.poll(), None, "empty file emits nothing");

        std::fs::write(&file, "/home/me/proj\n\n").unwrap();
        assert_eq!(p.poll().as_deref(), Some("/home/me/proj"));
        assert_eq!(p.poll(), None, "unchanged stamp is not re-read");

        // Same resolved value in a differently sized file: still no emit.
        std::fs::write(&file, "/home/me/proj\nextra-line-changes-len\n").unwrap();
        assert_eq!(p.poll(), None, "same cwd is not emitted twice");

        std::fs::write(&file, "/tmp\n").unwrap();
        assert_eq!(p.poll().as_deref(), Some("/tmp"));
    }

    #[test]
    fn windows_default_prefers_pwsh_on_path() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(windows_default_shell(Some(dir.path().into())), "powershell.exe");
        assert_eq!(windows_default_shell(None), "powershell.exe");

        std::fs::write(dir.path().join("pwsh.exe"), b"").unwrap();
        let path = std::env::join_paths([std::path::Path::new("/nonexistent"), dir.path()]).unwrap();
        assert_eq!(windows_default_shell(Some(path)), "pwsh.exe");
    }
}
