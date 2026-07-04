//! SSH shell sessions backed by russh (pure Rust — no OpenSSL/libssh2 to
//! link, which keeps Windows/macOS/Linux builds uniform).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};

use russh::client::{self, Handle};
use russh::keys::known_hosts::learn_known_hosts_path;
use russh::keys::{check_known_hosts_path, load_secret_key, HashAlg, PrivateKeyWithHashAlg, PublicKey};
use russh::{ChannelMsg, Disconnect};
use tokio::sync::Mutex as AsyncMutex;

use super::{EventSink, SessionError, SessionMap, SshSecrets};
use crate::models::{SshAuthMethod, SshHost};

/// Write side of an established SSH shell. Shared so the manager can await
/// writes without holding the session-map lock.
pub(crate) struct SshWriter {
    write_half: russh::ChannelWriteHalf<client::Msg>,
    handle: Handle<Client>,
}

pub(crate) struct SshSession {
    writer: Arc<AsyncMutex<SshWriter>>,
}

/// Outcome of the known_hosts check for the key presented during key
/// exchange. Stashed by `Client::check_server_key` (which can only return a
/// bool/`russh::Error`) so `SshSession::connect` can turn a failed handshake
/// into a precise, user-facing `SessionError` afterwards.
enum HostKeyOutcome {
    /// The host is not present in known_hosts at all.
    Unknown { key_type: String, fingerprint: String },
    /// The host is present, but under a *different* key — a possible MITM.
    Mismatch {
        line: usize,
        key_type: String,
        fingerprint: String,
    },
    /// known_hosts could not be read/parsed/written for some other reason.
    VerifyError(String),
}

/// Result of comparing a presented key against a known_hosts file, isolated
/// from russh's `Handler` plumbing so the matching logic can be unit tested
/// directly against a temp file.
#[derive(Debug, PartialEq)]
enum HostKeyCheck {
    Match,
    Unknown,
    Mismatch { line: usize },
    VerifyError(String),
}

fn check_host_key(host: &str, port: u16, pubkey: &PublicKey, known_hosts_path: &Path) -> HostKeyCheck {
    match check_known_hosts_path(host, port, pubkey, known_hosts_path) {
        Ok(true) => HostKeyCheck::Match,
        Ok(false) => HostKeyCheck::Unknown,
        Err(russh::keys::Error::KeyChanged { line }) => HostKeyCheck::Mismatch { line },
        Err(e) => HostKeyCheck::VerifyError(e.to_string()),
    }
}

/// `ssh-ed25519` style algorithm name and `SHA256:...` fingerprint, the same
/// shorthand OpenSSH prints when it meets an unfamiliar host key.
fn describe_key(pubkey: &PublicKey) -> (String, String) {
    (
        pubkey.algorithm().to_string(),
        pubkey.fingerprint(HashAlg::Sha256).to_string(),
    )
}

/// Default known_hosts location (`~/.ssh/known_hosts`), used whenever the
/// caller doesn't supply an explicit path (tests use a tempfile instead so
/// they never touch a real home directory).
pub(crate) fn default_known_hosts_path() -> PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    home.join(".ssh").join("known_hosts")
}

struct Client {
    host: String,
    port: u16,
    trust_host: bool,
    known_hosts_path: PathBuf,
    outcome: Arc<StdMutex<Option<HostKeyOutcome>>>,
}

impl client::Handler for Client {
    type Error = russh::Error;

    /// OpenSSH-compatible known_hosts (TOFU) verification:
    /// - key matches the recorded entry → accept
    /// - key present under a different key → **always** reject (possible
    ///   MITM), regardless of `trust_host`
    /// - host not recorded at all → accept only if `trust_host` is set (and
    ///   then record it), otherwise reject with enough detail (algorithm +
    ///   SHA256 fingerprint) for the caller to prompt the user
    async fn check_server_key(
        &mut self,
        server_public_key: &russh::keys::PublicKey,
    ) -> Result<bool, Self::Error> {
        match check_host_key(&self.host, self.port, server_public_key, &self.known_hosts_path) {
            HostKeyCheck::Match => Ok(true),
            HostKeyCheck::Mismatch { line } => {
                let (key_type, fingerprint) = describe_key(server_public_key);
                *self.outcome.lock().unwrap() = Some(HostKeyOutcome::Mismatch {
                    line,
                    key_type,
                    fingerprint,
                });
                Err(russh::Error::KeyChanged { line })
            }
            HostKeyCheck::VerifyError(msg) => {
                *self.outcome.lock().unwrap() = Some(HostKeyOutcome::VerifyError(msg));
                Ok(false)
            }
            HostKeyCheck::Unknown => {
                if self.trust_host {
                    if let Err(e) = learn_known_hosts_path(
                        &self.host,
                        self.port,
                        server_public_key,
                        &self.known_hosts_path,
                    ) {
                        *self.outcome.lock().unwrap() = Some(HostKeyOutcome::VerifyError(format!(
                            "known_hosts への書き込みに失敗しました: {e}"
                        )));
                        return Err(russh::Error::from(e));
                    }
                    Ok(true)
                } else {
                    let (key_type, fingerprint) = describe_key(server_public_key);
                    *self.outcome.lock().unwrap() = Some(HostKeyOutcome::Unknown {
                        key_type,
                        fingerprint,
                    });
                    Ok(false)
                }
            }
        }
    }
}

/// Turns a failed `client::connect` (whose only signal is `check_server_key`
/// returning `false`/`Err`) into the precise `SessionError` the UI needs,
/// using the details `Client::check_server_key` stashed along the way.
fn host_key_error(e: russh::Error, outcome: &StdMutex<Option<HostKeyOutcome>>) -> SessionError {
    match outcome.lock().unwrap().take() {
        Some(HostKeyOutcome::Unknown { key_type, fingerprint }) => SessionError::UnknownHostKey {
            key_type,
            fingerprint,
        },
        Some(HostKeyOutcome::Mismatch {
            line,
            key_type,
            fingerprint,
        }) => SessionError::HostKeyMismatch {
            line,
            key_type,
            fingerprint,
        },
        Some(HostKeyOutcome::VerifyError(msg)) => {
            SessionError::Ssh(format!("known_hosts の検証に失敗しました: {msg}"))
        }
        None => SessionError::Ssh(format!("connect failed: {e}")),
    }
}

impl SshSession {
    #[allow(clippy::too_many_arguments)]
    pub async fn connect(
        id: &str,
        host: &SshHost,
        secrets: SshSecrets,
        cols: u16,
        rows: u16,
        trust_host: bool,
        known_hosts_path: PathBuf,
        sink: Arc<dyn EventSink>,
        sessions: SessionMap,
    ) -> Result<Self, SessionError> {
        let config = Arc::new(client::Config {
            keepalive_interval: Some(std::time::Duration::from_secs(30)),
            ..Default::default()
        });

        let outcome = Arc::new(StdMutex::new(None));
        let handler = Client {
            host: host.host.clone(),
            port: host.port,
            trust_host,
            known_hosts_path,
            outcome: outcome.clone(),
        };

        let mut handle = client::connect(config, (host.host.as_str(), host.port), handler)
            .await
            .map_err(|e| host_key_error(e, &outcome))?;

        authenticate(&mut handle, host, &secrets).await?;

        let channel = handle
            .channel_open_session()
            .await
            .map_err(|e| SessionError::Ssh(format!("channel open failed: {e}")))?;
        channel
            .request_pty(false, "xterm-256color", cols as u32, rows as u32, 0, 0, &[])
            .await
            .map_err(|e| SessionError::Ssh(format!("pty request failed: {e}")))?;
        channel
            .request_shell(false)
            .await
            .map_err(|e| SessionError::Ssh(format!("shell request failed: {e}")))?;

        let (mut read_half, write_half) = channel.split();

        // Reader task: pump channel output to the sink until the channel
        // closes, then drop the session from the map and announce the exit.
        let sid = id.to_string();
        tokio::spawn(async move {
            let mut code = 0i32;
            while let Some(msg) = read_half.wait().await {
                match msg {
                    ChannelMsg::Data { data } => sink.data(&sid, &data),
                    ChannelMsg::ExtendedData { data, .. } => sink.data(&sid, &data),
                    ChannelMsg::ExitStatus { exit_status } => code = exit_status as i32,
                    _ => {}
                }
            }
            sessions.lock().unwrap().remove(&sid);
            sink.exit(&sid, code);
        });

        Ok(Self {
            writer: Arc::new(AsyncMutex::new(SshWriter { write_half, handle })),
        })
    }

    pub fn writer(&self) -> Arc<AsyncMutex<SshWriter>> {
        self.writer.clone()
    }

    pub async fn write(writer: &AsyncMutex<SshWriter>, data: &[u8]) -> Result<(), SessionError> {
        writer
            .lock()
            .await
            .write_half
            .data(data)
            .await
            .map_err(|e| SessionError::Ssh(e.to_string()))
    }

    pub async fn resize(
        writer: &AsyncMutex<SshWriter>,
        cols: u16,
        rows: u16,
    ) -> Result<(), SessionError> {
        writer
            .lock()
            .await
            .write_half
            .window_change(cols as u32, rows as u32, 0, 0)
            .await
            .map_err(|e| SessionError::Ssh(e.to_string()))
    }

    pub fn kill(self) {
        let writer = self.writer;
        tokio::spawn(async move {
            let guard = writer.lock().await;
            let _ = guard
                .handle
                .disconnect(Disconnect::ByApplication, "closed by user", "")
                .await;
        });
    }
}

async fn authenticate(
    handle: &mut Handle<Client>,
    host: &SshHost,
    secrets: &SshSecrets,
) -> Result<(), SessionError> {
    let auth = match host.auth_method {
        SshAuthMethod::Password => {
            let password = secrets.password.clone().unwrap_or_default();
            handle
                .authenticate_password(&host.username, password)
                .await
                .map_err(|e| SessionError::Ssh(format!("auth failed: {e}")))?
        }
        SshAuthMethod::Key => {
            let path = expand_home(&host.key_path);
            let key = load_secret_key(&path, secrets.passphrase.as_deref())
                .map_err(|e| SessionError::Ssh(format!("cannot load key {path}: {e}")))?;
            let hash = handle
                .best_supported_rsa_hash()
                .await
                .map_err(|e| SessionError::Ssh(e.to_string()))?
                .flatten();
            handle
                .authenticate_publickey(
                    &host.username,
                    PrivateKeyWithHashAlg::new(Arc::new(key), hash),
                )
                .await
                .map_err(|e| SessionError::Ssh(format!("auth failed: {e}")))?
        }
        SshAuthMethod::Agent => {
            return authenticate_with_agent(handle, host).await;
        }
    };
    if auth.success() {
        Ok(())
    } else {
        Err(SessionError::Ssh("authentication rejected".into()))
    }
}

async fn authenticate_with_agent(
    handle: &mut Handle<Client>,
    host: &SshHost,
) -> Result<(), SessionError> {
    #[cfg(unix)]
    let mut agent = russh::keys::agent::client::AgentClient::connect_env()
        .await
        .map_err(|e| SessionError::Ssh(format!("cannot reach ssh-agent: {e}")))?;
    // On Windows, OpenSSH's ssh-agent service listens on a named pipe rather
    // than a unix socket, so russh has no connect_env there.
    #[cfg(windows)]
    let mut agent = {
        let pipe_name = std::env::var("SSH_AUTH_SOCK")
            .unwrap_or_else(|_| r"\\.\pipe\openssh-ssh-agent".to_string());
        let pipe = tokio::net::windows::named_pipe::ClientOptions::new()
            .open(&pipe_name)
            .map_err(|e| {
                SessionError::Ssh(format!("cannot reach ssh-agent at {pipe_name}: {e}"))
            })?;
        russh::keys::agent::client::AgentClient::connect(pipe)
    };
    let identities = agent
        .request_identities()
        .await
        .map_err(|e| SessionError::Ssh(format!("ssh-agent error: {e}")))?;
    if identities.is_empty() {
        return Err(SessionError::Ssh("ssh-agent holds no identities".into()));
    }
    let hash = handle
        .best_supported_rsa_hash()
        .await
        .map_err(|e| SessionError::Ssh(e.to_string()))?
        .flatten();
    for key in identities {
        let auth = handle
            .authenticate_publickey_with(&host.username, key, hash, &mut agent)
            .await
            .map_err(|e| SessionError::Ssh(format!("auth failed: {e}")))?;
        if auth.success() {
            return Ok(());
        }
    }
    Err(SessionError::Ssh(
        "no ssh-agent identity was accepted".into(),
    ))
}

fn expand_home(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~") {
        if let Some(home) = dirs::home_dir() {
            return format!("{}{}", home.display(), rest);
        }
    }
    path.to_string()
}

#[cfg(test)]
mod known_hosts_tests {
    use super::*;
    use std::io::Write;
    use russh::keys::parse_public_key_base64;

    // Two arbitrary, unrelated ed25519 public keys (base64 payload only, no
    // matching private key needed for these tests).
    const KEY_A: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIJdD7y3aLq454yWBdwLWbieU1ebz9/cu7/QEXn9OIeZJ";
    const KEY_B: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIA6rWI3G1sz07DnfFlrouTcysQlj2P+jpNSOEWD9OJ3X";

    fn key(b64: &str) -> PublicKey {
        parse_public_key_base64(b64).unwrap()
    }

    fn known_hosts_with(contents: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("known_hosts");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(contents.as_bytes()).unwrap();
        (dir, path)
    }

    #[test]
    fn unknown_host_when_known_hosts_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does-not-exist");
        let check = check_host_key("example.com", 22, &key(KEY_A), &path);
        assert_eq!(check, HostKeyCheck::Unknown);
    }

    #[test]
    fn unknown_host_when_no_entry_matches() {
        let (_dir, path) = known_hosts_with(&format!("otherhost ssh-ed25519 {KEY_A}\n"));
        let check = check_host_key("example.com", 22, &key(KEY_A), &path);
        assert_eq!(check, HostKeyCheck::Unknown);
    }

    #[test]
    fn matches_recorded_key() {
        let (_dir, path) = known_hosts_with(&format!("example.com ssh-ed25519 {KEY_A}\n"));
        let check = check_host_key("example.com", 22, &key(KEY_A), &path);
        assert_eq!(check, HostKeyCheck::Match);
    }

    #[test]
    fn matches_recorded_key_on_nonstandard_port() {
        let (_dir, path) = known_hosts_with(&format!("[example.com]:2222 ssh-ed25519 {KEY_A}\n"));
        let check = check_host_key("example.com", 2222, &key(KEY_A), &path);
        assert_eq!(check, HostKeyCheck::Match);
    }

    #[test]
    fn mismatch_when_recorded_key_differs() {
        let (_dir, path) = known_hosts_with(&format!("example.com ssh-ed25519 {KEY_A}\n"));
        let check = check_host_key("example.com", 22, &key(KEY_B), &path);
        assert_eq!(check, HostKeyCheck::Mismatch { line: 1 });
    }

    #[test]
    fn mismatch_never_falls_back_to_accepting() {
        // A changed key must never be silently treated as a match or as an
        // unknown host — always a hard rejection.
        let (_dir, path) = known_hosts_with(&format!(
            "a.example ssh-ed25519 {KEY_A}\nexample.com ssh-ed25519 {KEY_A}\n"
        ));
        let check = check_host_key("example.com", 22, &key(KEY_B), &path);
        assert_eq!(check, HostKeyCheck::Mismatch { line: 2 });
    }

    #[test]
    fn describe_key_reports_algorithm_and_sha256_fingerprint() {
        let (key_type, fingerprint) = describe_key(&key(KEY_A));
        assert_eq!(key_type, "ssh-ed25519");
        assert!(
            fingerprint.starts_with("SHA256:"),
            "unexpected fingerprint format: {fingerprint}"
        );
    }

    #[test]
    fn learn_then_match() {
        let (_dir, path) = known_hosts_with("");
        assert_eq!(
            check_host_key("example.com", 22, &key(KEY_A), &path),
            HostKeyCheck::Unknown
        );
        learn_known_hosts_path("example.com", 22, &key(KEY_A), &path).unwrap();
        assert_eq!(
            check_host_key("example.com", 22, &key(KEY_A), &path),
            HostKeyCheck::Match
        );
        // A different key is still rejected as a mismatch once learned.
        // (`learn_known_hosts_path` prefixes the entry with a blank line when
        // starting from a completely empty file, so the entry lands on line 2.)
        assert_eq!(
            check_host_key("example.com", 22, &key(KEY_B), &path),
            HostKeyCheck::Mismatch { line: 2 }
        );
    }
}
