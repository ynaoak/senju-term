//! Unified management of interactive sessions. Local shells (via
//! portable-pty, which uses ConPTY on Windows and openpty on Unix) and SSH
//! shells (via russh, pure Rust) are exposed behind one id-based interface so
//! the UI layer only deals with a single data/exit event stream.

mod local;
mod shell_integration;
mod ssh;

pub use ssh::SshTestReport;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::Serialize;

use crate::models::SshHost;

/// Receives terminal output and lifecycle events; implemented by the GUI
/// layer (e.g. forwarding to Tauri window events).
pub trait EventSink: Send + Sync + 'static {
    fn data(&self, id: &str, data: &[u8]);
    fn exit(&self, id: &str, code: i32);
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionInfo {
    pub id: String,
    pub title: String,
    pub kind: String,
}

/// Secrets collected in the UI at connect time. Never persisted.
#[derive(Default, Clone)]
pub struct SshSecrets {
    pub password: Option<String>,
    pub passphrase: Option<String>,
}

// Redacting Debug so a stray `{:?}`/log line can never spill the password or
// passphrase (only whether each is present).
impl std::fmt::Debug for SshSecrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SshSecrets")
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .field("passphrase", &self.passphrase.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

/// How to launch a local shell — the resolved form of a terminal profile.
#[derive(Debug, Default, Clone)]
pub struct LocalSpec {
    /// Executable; empty means the OS default shell.
    pub command: String,
    pub args: Vec<String>,
    /// Working directory; empty means the home directory. `~` expands.
    pub cwd: String,
    /// Display title; empty derives one from the executable name.
    pub title: String,
}

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("{0}")]
    Pty(String),
    #[error("{0}")]
    Ssh(String),
    #[error("unknown session: {0}")]
    UnknownSession(String),
    /// The server's host key isn't recorded in known_hosts at all. The
    /// message keeps the `UNKNOWN_HOST_KEY:` prefix machine-parseable (the
    /// frontend uses it to offer a "trust and save" prompt) while still
    /// carrying the algorithm and SHA256 fingerprint a human should verify.
    #[error("UNKNOWN_HOST_KEY:{key_type}:{fingerprint}")]
    UnknownHostKey { key_type: String, fingerprint: String },
    /// The server presented a key that does NOT match the one recorded in
    /// known_hosts for this host — a possible man-in-the-middle attack.
    /// Always rejected, independent of any "trust this host" request.
    #[error(
        "known_hosts に記録された鍵と一致しません(MITM の可能性があります)。known_hosts の {line} 行目を確認してください。鍵種別={key_type} フィンガープリント={fingerprint}"
    )]
    HostKeyMismatch {
        line: usize,
        key_type: String,
        fingerprint: String,
    },
    /// The host was still unknown, the UI had already shown the user a
    /// fingerprint from a *previous* handshake and the user approved it, but
    /// the key presented on THIS (re-)connection has a different SHA256
    /// fingerprint. This is the TOFU TOCTOU window a MITM could exploit by
    /// relaying the real key on the first handshake (so the user approves a
    /// genuine fingerprint) and substituting its own key on the second. The
    /// key is never learned and the connection is always rejected — there is
    /// no fallback that accepts it. The `FINGERPRINT_MISMATCH:` prefix keeps
    /// the message machine-parseable for the frontend, same convention as
    /// `UnknownHostKey`.
    #[error(
        "FINGERPRINT_MISMATCH:{key_type}:{actual}:承認したホスト鍵(フィンガープリント={expected})とは異なる鍵が提示されました。中間者攻撃(MITM)の可能性があるため接続を中止しました。known_hosts への保存は行っていません。"
    )]
    FingerprintMismatch {
        key_type: String,
        expected: String,
        actual: String,
    },
}

pub(crate) enum Backend {
    Local(local::LocalSession),
    Ssh(ssh::SshSession),
}

type SessionMap = Arc<Mutex<HashMap<String, Backend>>>;

pub struct SessionManager {
    sink: Arc<dyn EventSink>,
    sessions: SessionMap,
    /// Overrides the known_hosts file SSH connections verify against.
    /// `None` means the real `~/.ssh/known_hosts`; tests point this at a
    /// tempfile so they never touch (or depend on) a real home directory.
    known_hosts_path: Option<PathBuf>,
}

impl SessionManager {
    pub fn new(sink: Arc<dyn EventSink>) -> Self {
        Self::with_known_hosts_path(sink, None)
    }

    /// Like [`Self::new`], but pins the known_hosts file used for SSH host
    /// key verification to `known_hosts_path` instead of `~/.ssh/known_hosts`.
    pub fn with_known_hosts_path(sink: Arc<dyn EventSink>, known_hosts_path: Option<PathBuf>) -> Self {
        Self {
            sink,
            sessions: Arc::new(Mutex::new(HashMap::new())),
            known_hosts_path,
        }
    }

    /// Spawns a local shell described by `spec` (a resolved terminal profile).
    /// `shell_integration` opts into auto-injecting OSC 133 hooks (see
    /// `shell_integration` module) for a recognized shell (bash/zsh/fish).
    pub fn create_local(
        &self,
        spec: &LocalSpec,
        cols: u16,
        rows: u16,
        shell_integration: bool,
    ) -> Result<SessionInfo, SessionError> {
        let id = uuid::Uuid::new_v4().to_string();
        let (session, title) = local::LocalSession::spawn(
            &id,
            spec,
            cols,
            rows,
            shell_integration,
            self.sink.clone(),
            self.sessions.clone(),
        )?;
        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.clone(), Backend::Local(session));
        Ok(SessionInfo {
            id,
            title,
            kind: "local".into(),
        })
    }

    /// Connects to a saved SSH host and opens a shell channel.
    ///
    /// `expected_fingerprint` controls what happens when the server's host
    /// key is not yet recorded in known_hosts:
    /// - `None` fails the connection with [`SessionError::UnknownHostKey`]
    ///   (the caller can show the fingerprint in a TOFU prompt and, if the
    ///   user approves it, retry with `Some(that_fingerprint)`);
    /// - `Some(fp)` only records the key and proceeds if the key presented
    ///   during THIS handshake has a SHA256 fingerprint exactly equal to
    ///   `fp`. If it doesn't match, the connection fails with
    ///   [`SessionError::FingerprintMismatch`] and nothing is written to
    ///   known_hosts — this closes the TOCTOU gap where a MITM could relay
    ///   the real key on the first handshake (so the user approves a genuine
    ///   fingerprint) and substitute its own key on the second.
    ///
    /// A key that mismatches an *existing* known_hosts entry is always
    /// rejected via [`SessionError::HostKeyMismatch`], regardless of
    /// `expected_fingerprint`.
    pub async fn create_ssh(
        &self,
        host: &SshHost,
        secrets: SshSecrets,
        cols: u16,
        rows: u16,
        expected_fingerprint: Option<String>,
    ) -> Result<SessionInfo, SessionError> {
        let id = uuid::Uuid::new_v4().to_string();
        let known_hosts_path = self
            .known_hosts_path
            .clone()
            .unwrap_or_else(ssh::default_known_hosts_path);
        let session = ssh::SshSession::connect(
            &id,
            host,
            secrets,
            cols,
            rows,
            expected_fingerprint,
            known_hosts_path,
            self.sink.clone(),
            self.sessions.clone(),
        )
        .await?;
        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.clone(), Backend::Ssh(session));
        Ok(SessionInfo {
            id,
            title: if host.name.is_empty() {
                format!("{}@{}", host.username, host.host)
            } else {
                host.name.clone()
            },
            kind: "ssh".into(),
        })
    }

    /// Probes an SSH host (reachability, host key, credentials) without
    /// creating a persistent session or learning the host key. Backs the
    /// pre-save "test connection" button in the host editor.
    ///
    /// Like [`Self::create_ssh`], credentials are only sent once the host key
    /// is verified: on first contact with an unrecorded host this fails with
    /// [`SessionError::UnknownHostKey`] before authenticating, so the UI can
    /// show the fingerprint and retry with `expected_fingerprint`.
    pub async fn test_ssh(
        &self,
        host: &SshHost,
        secrets: SshSecrets,
        expected_fingerprint: Option<String>,
    ) -> Result<SshTestReport, SessionError> {
        let known_hosts_path = self
            .known_hosts_path
            .clone()
            .unwrap_or_else(ssh::default_known_hosts_path);
        ssh::test_connection(host, secrets, expected_fingerprint, known_hosts_path).await
    }

    pub async fn write(&self, id: &str, data: &[u8]) -> Result<(), SessionError> {
        enum Target {
            Local(local::LocalWriter),
            Ssh(Arc<tokio::sync::Mutex<ssh::SshWriter>>),
        }
        // Only look the session up under the lock; the actual write happens
        // after it is released so a slow/blocked writer can't stall every other
        // session op (Q2). Local writes are blocking, so they go to a blocking
        // thread; SSH writes are async.
        let target = {
            let mut map = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
            match map.get_mut(id) {
                Some(Backend::Local(s)) => Target::Local(s.writer_handle()),
                Some(Backend::Ssh(s)) => Target::Ssh(s.writer()),
                None => return Err(SessionError::UnknownSession(id.into())),
            }
        };
        match target {
            Target::Local(writer) => {
                let data = data.to_vec();
                tokio::task::spawn_blocking(move || {
                    local::LocalSession::write_blocking(&writer, &data)
                })
                .await
                .map_err(|e| SessionError::Pty(format!("write task failed: {e}")))?
            }
            Target::Ssh(writer) => ssh::SshSession::write(&writer, data).await,
        }
    }

    pub async fn resize(&self, id: &str, cols: u16, rows: u16) -> Result<(), SessionError> {
        let target = {
            let mut map = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
            match map.get_mut(id) {
                Some(Backend::Local(s)) => return s.resize(cols, rows),
                Some(Backend::Ssh(s)) => s.writer(),
                None => return Err(SessionError::UnknownSession(id.into())),
            }
        };
        ssh::SshSession::resize(&target, cols, rows).await
    }

    /// Terminates the session. The exit event is emitted by the session's own
    /// reader task once the underlying stream closes.
    pub fn kill(&self, id: &str) {
        let backend = self.sessions.lock().unwrap_or_else(|e| e.into_inner()).remove(id);
        match backend {
            Some(Backend::Local(s)) => s.kill(),
            Some(Backend::Ssh(s)) => s.kill(),
            None => {}
        }
    }

    pub fn kill_all(&self) {
        let ids: Vec<String> = self.sessions.lock().unwrap_or_else(|e| e.into_inner()).keys().cloned().collect();
        for id in ids {
            self.kill(&id);
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;
    use std::time::{Duration, Instant};

    #[derive(Default)]
    struct Capture {
        data: StdMutex<Vec<u8>>,
        exited: StdMutex<Option<i32>>,
    }

    impl EventSink for Capture {
        fn data(&self, _id: &str, data: &[u8]) {
            self.data.lock().unwrap().extend_from_slice(data);
        }
        fn exit(&self, _id: &str, code: i32) {
            *self.exited.lock().unwrap() = Some(code);
        }
    }

    fn wait_until(deadline: Duration, mut cond: impl FnMut() -> bool) -> bool {
        let start = Instant::now();
        while start.elapsed() < deadline {
            if cond() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    fn sh_spec() -> LocalSpec {
        LocalSpec {
            command: "/bin/sh".into(),
            ..Default::default()
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn local_session_echoes_and_exits() {
        let sink = Arc::new(Capture::default());
        let mgr = SessionManager::new(sink.clone());
        let info = mgr.create_local(&sh_spec(), 80, 24, false).unwrap();
        assert_eq!(info.kind, "local");

        mgr.write(&info.id, b"echo senju-$((20+3))\n").await.unwrap();
        assert!(
            wait_until(Duration::from_secs(10), || String::from_utf8_lossy(
                &sink.data.lock().unwrap()
            )
            .contains("senju-23")),
            "expected echoed output, got: {}",
            String::from_utf8_lossy(&sink.data.lock().unwrap())
        );

        mgr.resize(&info.id, 100, 30).await.unwrap();
        mgr.write(&info.id, b"exit\n").await.unwrap();
        assert!(wait_until(Duration::from_secs(10), || sink
            .exited
            .lock()
            .unwrap()
            .is_some()));
    }

    /// End-to-end: a real bash session with `shell_integration: true` should
    /// actually emit OSC 133 markers when a command runs — not just the unit
    /// tests inside the `shell_integration` module (which only check the
    /// generated rcfile's *content*, not that bash really reads it).
    #[tokio::test(flavor = "multi_thread")]
    async fn shell_integration_makes_bash_emit_osc_133() {
        let sink = Arc::new(Capture::default());
        let mgr = SessionManager::new(sink.clone());
        let spec = LocalSpec { command: "bash".into(), ..Default::default() };
        let info = mgr.create_local(&spec, 80, 24, true).unwrap();

        fn has(haystack: &[u8], marker: &[u8]) -> bool {
            haystack.windows(marker.len()).any(|w| w == marker)
        }

        mgr.write(&info.id, b"echo hi\n").await.unwrap();
        assert!(
            wait_until(Duration::from_secs(10), || has(
                &sink.data.lock().unwrap(),
                b"\x1b]133;D"
            )),
            "expected an OSC 133;D (command-done) marker, got: {}",
            String::from_utf8_lossy(&sink.data.lock().unwrap())
        );
        let out = sink.data.lock().unwrap();
        for marker in [&b"\x1b]133;A"[..], b"\x1b]133;B", b"\x1b]133;C"] {
            assert!(
                has(&out, marker),
                "missing {marker:?} in: {}",
                String::from_utf8_lossy(&out)
            );
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn kill_terminates_session() {
        let sink = Arc::new(Capture::default());
        let mgr = SessionManager::new(sink.clone());
        let info = mgr.create_local(&sh_spec(), 80, 24, false).unwrap();
        mgr.kill(&info.id);
        assert!(wait_until(Duration::from_secs(10), || sink
            .exited
            .lock()
            .unwrap()
            .is_some()));
        assert!(matches!(
            mgr.write(&info.id, b"x").await,
            Err(SessionError::UnknownSession(_))
        ));
    }
}
