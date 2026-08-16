//! Local shell sessions backed by portable-pty (ConPTY on Windows, openpty
//! on Unix — the same crate WezTerm uses).

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

use portable_pty::{native_pty_system, Child, ChildKiller, CommandBuilder, MasterPty, PtySize};

use super::shell_integration;
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
}

impl LocalReader {
    /// Pumps PTY output to the sink until EOF, then reaps the child, drops the
    /// session from the map and announces the exit. Call only after the
    /// session has been inserted into `sessions`.
    pub fn start(self, id: String, sink: Arc<dyn EventSink>, sessions: SessionMap) {
        let LocalReader { mut reader, mut child } = self;
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
            sink.exit(&id, code);
        });
    }
}

pub(crate) struct LocalSession {
    master: Mutex<Box<dyn MasterPty + Send>>,
    writer: LocalWriter,
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    // Keeps the generated shell-integration rcfile/ZDOTDIR alive for the
    // session's lifetime — the shell only reads it at startup, but holding it
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

        // Best-effort: a recognized shell (bash/zsh/fish) gets OSC 133 hooks
        // spliced into its launch, so the frontend's command-block UI works
        // without the user editing any rc file themselves.
        let integration = shell_integration_enabled
            .then(|| shell_integration::build(&shell, &spec.args))
            .flatten();

        let mut cmd = CommandBuilder::new(&shell);
        if let Some(integ) = &integration {
            for arg in &integ.prefix_args {
                cmd.arg(arg);
            }
        }
        for arg in &spec.args {
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
        Ok((
            Self {
                master: Mutex::new(pair.master),
                writer: Arc::new(Mutex::new(writer)),
                killer: Mutex::new(killer),
                _integration_guard: integration.and_then(|i| i.guard),
            },
            title,
            LocalReader { reader, child },
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
        std::env::var("COMSPEC").unwrap_or_else(|_| "powershell.exe".into())
    } else {
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into())
    }
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
