//! Local shell sessions backed by portable-pty (ConPTY on Windows, openpty
//! on Unix — the same crate WezTerm uses).

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};

use super::{EventSink, SessionError, SessionMap};

pub(crate) struct LocalSession {
    master: Mutex<Box<dyn MasterPty + Send>>,
    writer: Mutex<Box<dyn Write + Send>>,
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
}

impl LocalSession {
    pub fn spawn(
        id: &str,
        shell_override: &str,
        cols: u16,
        rows: u16,
        sink: Arc<dyn EventSink>,
        sessions: SessionMap,
    ) -> Result<(Self, String), SessionError> {
        let shell = if shell_override.is_empty() {
            default_shell()
        } else {
            shell_override.to_string()
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

        let mut cmd = CommandBuilder::new(&shell);
        cmd.env("TERM", "xterm-256color");
        cmd.env("TERM_PROGRAM", "senju-term");
        if let Some(home) = dirs::home_dir() {
            cmd.cwd(home);
        }

        let mut child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| SessionError::Pty(e.to_string()))?;
        drop(pair.slave);

        let killer = child.clone_killer();
        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| SessionError::Pty(e.to_string()))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| SessionError::Pty(e.to_string()))?;

        // Reader thread: pump PTY output to the sink until EOF, then reap the
        // child, drop the session from the map and announce the exit.
        let sid = id.to_string();
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => sink.data(&sid, &buf[..n]),
                }
            }
            let code = child.wait().map(|s| s.exit_code() as i32).unwrap_or(-1);
            sessions.lock().unwrap().remove(&sid);
            sink.exit(&sid, code);
        });

        let title = shell
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(&shell)
            .to_string();
        Ok((
            Self {
                master: Mutex::new(pair.master),
                writer: Mutex::new(writer),
                killer: Mutex::new(killer),
            },
            title,
        ))
    }

    pub fn write(&self, data: &[u8]) -> Result<(), SessionError> {
        self.writer
            .lock()
            .unwrap()
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
