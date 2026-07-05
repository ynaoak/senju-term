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
    /// The Tokio runtime handle captured at connect time (inside the async
    /// context). `kill` spawns the graceful-disconnect task onto this handle
    /// rather than the ambient runtime, so it is safe to call from a plain
    /// thread — e.g. Tauri's window `Destroyed` handler, which runs on the
    /// GUI thread outside any runtime. Using `tokio::spawn` there would panic
    /// with "there is no reactor running".
    runtime: tokio::runtime::Handle,
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
    /// The host is still unknown, and the caller supplied a fingerprint the
    /// user approved during a *previous* handshake (the first TOFU prompt
    /// round-trip), but the key presented THIS handshake doesn't match it.
    /// This is exactly the TOCTOU window a MITM could exploit by relaying the
    /// real key on the first handshake and substituting its own on the
    /// second — so it is never learned and never accepted.
    FingerprintMismatch {
        key_type: String,
        expected: String,
        actual: String,
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

/// Result of resolving a `HostKeyCheck::Unknown` host against an
/// `expected_fingerprint` (or the lack of one). This is the crux of the
/// TOCTOU fix: it is a plain, synchronous function — independent of russh's
/// async `Handler` plumbing — so the fingerprint-matching logic itself can
/// be unit tested directly against a temp known_hosts file, with no fake SSH
/// server required.
#[derive(Debug, PartialEq)]
enum UnknownHostResolution {
    /// The presented key's own SHA256 fingerprint matched
    /// `expected_fingerprint` exactly; it has been learned into known_hosts.
    Learned,
    /// No `expected_fingerprint` was supplied at all: report the key back to
    /// the caller (typically to show a TOFU confirmation prompt) rather than
    /// auto-trusting it.
    ReportUnknown { key_type: String, fingerprint: String },
    /// An `expected_fingerprint` was supplied but did NOT match the key
    /// actually presented. **Fail closed**: nothing is learned or accepted.
    /// This is exactly the case a MITM hits by relaying the real key on a
    /// first handshake (which the user approves) and substituting its own
    /// key on the second.
    FingerprintMismatch {
        key_type: String,
        expected: String,
        actual: String,
    },
    /// known_hosts could not be written.
    WriteError(String),
}

fn resolve_unknown_host(
    host: &str,
    port: u16,
    pubkey: &PublicKey,
    known_hosts_path: &Path,
    expected_fingerprint: Option<&str>,
) -> UnknownHostResolution {
    let (key_type, actual_fingerprint) = describe_key(pubkey);
    match expected_fingerprint {
        Some(expected) if expected == actual_fingerprint => {
            match learn_known_hosts_path(host, port, pubkey, known_hosts_path) {
                Ok(()) => UnknownHostResolution::Learned,
                Err(e) => UnknownHostResolution::WriteError(format!(
                    "known_hosts への書き込みに失敗しました: {e}"
                )),
            }
        }
        Some(expected) => UnknownHostResolution::FingerprintMismatch {
            key_type,
            expected: expected.to_string(),
            actual: actual_fingerprint,
        },
        None => UnknownHostResolution::ReportUnknown {
            key_type,
            fingerprint: actual_fingerprint,
        },
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
    /// `Some(fp)` means: the host was unknown on a *prior* handshake, the UI
    /// showed the user fingerprint `fp`, and the user approved it. The key
    /// presented on THIS handshake is only trusted (and learned) if its own
    /// SHA256 fingerprint matches `fp` exactly — this is what closes the
    /// TOCTOU gap between "user looked at a fingerprint" and "we saved a
    /// key". `None` means no prior approval exists; an unknown host must be
    /// reported back to the UI instead of being auto-trusted.
    expected_fingerprint: Option<String>,
    known_hosts_path: PathBuf,
    outcome: Arc<StdMutex<Option<HostKeyOutcome>>>,
}

impl client::Handler for Client {
    type Error = russh::Error;

    /// OpenSSH-compatible known_hosts (TOFU) verification:
    /// - key matches the recorded entry → accept
    /// - key present under a different key → **always** reject (possible
    ///   MITM), regardless of `expected_fingerprint`
    /// - host not recorded at all:
    ///   - `expected_fingerprint` is `Some(fp)` and the presented key's own
    ///     SHA256 fingerprint equals `fp` exactly → learn it and accept
    ///     (the key the user approved is provably the key we are about to
    ///     trust)
    ///   - `expected_fingerprint` is `Some(fp)` but the fingerprints differ →
    ///     **fail closed**: never learn, never accept. This is the case a
    ///     MITM would hit by presenting a different key on the second
    ///     handshake than the one relayed (and approved) on the first.
    ///   - `expected_fingerprint` is `None` → reject with enough detail
    ///     (algorithm + SHA256 fingerprint) for the caller to prompt the user
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
            HostKeyCheck::Unknown => match resolve_unknown_host(
                &self.host,
                self.port,
                server_public_key,
                &self.known_hosts_path,
                self.expected_fingerprint.as_deref(),
            ) {
                UnknownHostResolution::Learned => Ok(true),
                UnknownHostResolution::FingerprintMismatch {
                    key_type,
                    expected,
                    actual,
                } => {
                    // The key presented now does NOT match the fingerprint
                    // the user approved earlier. Never learn it, never
                    // accept it — no `Ok(true)` fallback here.
                    *self.outcome.lock().unwrap() = Some(HostKeyOutcome::FingerprintMismatch {
                        key_type,
                        expected,
                        actual,
                    });
                    Ok(false)
                }
                UnknownHostResolution::ReportUnknown { key_type, fingerprint } => {
                    *self.outcome.lock().unwrap() = Some(HostKeyOutcome::Unknown {
                        key_type,
                        fingerprint,
                    });
                    Ok(false)
                }
                UnknownHostResolution::WriteError(msg) => {
                    *self.outcome.lock().unwrap() = Some(HostKeyOutcome::VerifyError(msg.clone()));
                    Err(russh::Error::from(std::io::Error::other(msg)))
                }
            },
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
        Some(HostKeyOutcome::FingerprintMismatch {
            key_type,
            expected,
            actual,
        }) => SessionError::FingerprintMismatch {
            key_type,
            expected,
            actual,
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
        expected_fingerprint: Option<String>,
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
            expected_fingerprint,
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
            runtime: tokio::runtime::Handle::current(),
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

    /// Sends a graceful SSH disconnect in the background. Safe to call from
    /// any thread, including outside a Tokio runtime (e.g. the GUI thread on
    /// window close): the disconnect is spawned onto the runtime handle
    /// captured at connect time, not the ambient one. Even if the spawned
    /// task never runs (e.g. the runtime is already shutting down at process
    /// exit), dropping `writer` closes the channel and connection.
    pub fn kill(self) {
        let writer = self.writer;
        self.runtime.spawn(async move {
            let guard = writer.lock().await;
            let _ = guard
                .handle
                .disconnect(Disconnect::ByApplication, "closed by user", "")
                .await;
        });
    }
}

// Generic over the handler so both the real session (`Client`) and the
// pre-save connection test (`TestClient`) share one authentication path.
async fn authenticate<H: client::Handler>(
    handle: &mut Handle<H>,
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
            publickey_step(handle, host, secrets.passphrase.as_deref()).await?
        }
        SshAuthMethod::Agent => {
            return authenticate_with_agent(handle, host).await;
        }
        SshAuthMethod::KeyPassword => {
            // Multi-step auth for servers with `AuthenticationMethods
            // publickey,password`. Offer the public key first; if the server
            // is fully satisfied, we're done. Otherwise (partial success —
            // it accepted the key but still wants a password) continue with
            // the password to complete authentication.
            let r = publickey_step(handle, host, secrets.passphrase.as_deref()).await?;
            if r.success() {
                return Ok(());
            }
            let password = secrets.password.clone().unwrap_or_default();
            handle
                .authenticate_password(&host.username, password)
                .await
                .map_err(|e| SessionError::Ssh(format!("auth failed: {e}")))?
        }
    };
    if auth.success() {
        Ok(())
    } else {
        Err(SessionError::Ssh("authentication rejected".into()))
    }
}

/// Loads the host's private key (honoring `passphrase` for encrypted keys)
/// and offers it for public-key authentication, returning the server's
/// `AuthResult`. Shared by the `Key` and `KeyPassword` methods.
async fn publickey_step<H: client::Handler>(
    handle: &mut Handle<H>,
    host: &SshHost,
    passphrase: Option<&str>,
) -> Result<client::AuthResult, SessionError> {
    let path = expand_home(&host.key_path);
    let key = load_secret_key(&path, passphrase)
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
        .map_err(|e| SessionError::Ssh(format!("auth failed: {e}")))
}

async fn authenticate_with_agent<H: client::Handler>(
    handle: &mut Handle<H>,
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

/// Diagnostic result of a pre-save "test connection", surfaced to the SSH host
/// editor so the user can confirm reachability, credentials and the host key
/// before committing the host.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SshTestReport {
    /// The server host key's algorithm (e.g. `ssh-ed25519`).
    pub key_type: String,
    /// The server host key's `SHA256:...` fingerprint.
    pub fingerprint: String,
    /// True if this key already matched a known_hosts entry; false if the host
    /// is new (the test does NOT record it — that only happens on a real
    /// connect after the user approves the TOFU prompt).
    pub host_key_known: bool,
}

/// Host-key handler used only for the pre-save connection test. Unlike the
/// real [`Client`], an *unknown* host key is accepted for the probe (nothing
/// is ever written to known_hosts) so that authentication can be exercised;
/// the observed key is recorded for reporting. A key that mismatches an
/// EXISTING known_hosts entry still fails — that's a genuine MITM signal the
/// test must surface rather than hide.
struct TestClient {
    host: String,
    port: u16,
    known_hosts_path: PathBuf,
    observed: Arc<StdMutex<Option<(String, String, bool)>>>,
    mismatch: Arc<StdMutex<Option<usize>>>,
}

impl client::Handler for TestClient {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &PublicKey,
    ) -> Result<bool, Self::Error> {
        let (key_type, fingerprint) = describe_key(server_public_key);
        match check_host_key(&self.host, self.port, server_public_key, &self.known_hosts_path) {
            HostKeyCheck::Match => {
                *self.observed.lock().unwrap() = Some((key_type, fingerprint, true));
                Ok(true)
            }
            // Accept for the probe only; never learn during a test.
            HostKeyCheck::Unknown => {
                *self.observed.lock().unwrap() = Some((key_type, fingerprint, false));
                Ok(true)
            }
            HostKeyCheck::Mismatch { line } => {
                *self.observed.lock().unwrap() = Some((key_type, fingerprint, false));
                *self.mismatch.lock().unwrap() = Some(line);
                Err(russh::Error::KeyChanged { line })
            }
            // known_hosts unreadable: still let the probe proceed so auth can
            // be tested, and report the key as not-yet-known.
            HostKeyCheck::VerifyError(_) => {
                *self.observed.lock().unwrap() = Some((key_type, fingerprint, false));
                Ok(true)
            }
        }
    }
}

/// Probes a host WITHOUT creating a persistent session or learning the host
/// key: connects, authenticates, opens a session channel, then disconnects.
/// Backs the "接続テスト" button in the SSH host editor.
pub async fn test_connection(
    host: &SshHost,
    secrets: SshSecrets,
    known_hosts_path: PathBuf,
) -> Result<SshTestReport, SessionError> {
    let config = Arc::new(client::Config {
        keepalive_interval: Some(std::time::Duration::from_secs(30)),
        ..Default::default()
    });
    let observed = Arc::new(StdMutex::new(None));
    let mismatch = Arc::new(StdMutex::new(None));
    let handler = TestClient {
        host: host.host.clone(),
        port: host.port,
        known_hosts_path,
        observed: observed.clone(),
        mismatch: mismatch.clone(),
    };

    let mut handle = client::connect(config, (host.host.as_str(), host.port), handler)
        .await
        .map_err(|e| match mismatch.lock().unwrap().take() {
            Some(line) => {
                let (key_type, fingerprint) = observed
                    .lock()
                    .unwrap()
                    .clone()
                    .map(|(k, f, _)| (k, f))
                    .unwrap_or_default();
                SessionError::HostKeyMismatch {
                    line,
                    key_type,
                    fingerprint,
                }
            }
            None => SessionError::Ssh(format!("接続に失敗しました: {e}")),
        })?;

    authenticate(&mut handle, host, &secrets).await?;

    // Confirm a shell channel can actually be opened, then disconnect — the
    // test leaves no session behind.
    handle
        .channel_open_session()
        .await
        .map_err(|e| SessionError::Ssh(format!("チャネルを開けませんでした: {e}")))?;
    let _ = handle
        .disconnect(Disconnect::ByApplication, "connection test", "")
        .await;

    let (key_type, fingerprint, host_key_known) =
        observed.lock().unwrap().clone().unwrap_or_default();
    Ok(SshTestReport {
        key_type,
        fingerprint,
        host_key_known,
    })
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

    // -- TOFU TOCTOU fix: `resolve_unknown_host` ----------------------------
    //
    // These exercise the exact logic `Client::check_server_key` delegates to
    // for an unknown host, against a real (temp-file) known_hosts, without
    // needing a fake SSH server: does supplying the fingerprint the user
    // approved cause the key to be learned only when it truly matches what
    // was presented, and does a mismatch leave known_hosts untouched?

    #[test]
    fn expected_fingerprint_matching_learns_and_accepts() {
        let (_dir, path) = known_hosts_with("");
        let (_key_type, fingerprint) = describe_key(&key(KEY_A));

        let resolution =
            resolve_unknown_host("example.com", 22, &key(KEY_A), &path, Some(&fingerprint));
        assert_eq!(resolution, UnknownHostResolution::Learned);

        // The key must actually have been written and now verifies as a
        // match on a fresh check.
        assert_eq!(
            check_host_key("example.com", 22, &key(KEY_A), &path),
            HostKeyCheck::Match
        );
    }

    #[test]
    fn expected_fingerprint_mismatch_never_learns_or_accepts() {
        let (_dir, path) = known_hosts_with("");
        // `expected` is the fingerprint of KEY_A (what the user approved,
        // e.g. from a prior UNKNOWN_HOST_KEY prompt) but the server
        // presents KEY_B on this handshake — the TOCTOU/MITM scenario.
        let (_key_type, expected_fingerprint) = describe_key(&key(KEY_A));
        let (key_b_type, key_b_fingerprint) = describe_key(&key(KEY_B));

        let resolution = resolve_unknown_host(
            "example.com",
            22,
            &key(KEY_B),
            &path,
            Some(&expected_fingerprint),
        );
        assert_eq!(
            resolution,
            UnknownHostResolution::FingerprintMismatch {
                key_type: key_b_type,
                expected: expected_fingerprint,
                actual: key_b_fingerprint,
            }
        );

        // Nothing must have been written to known_hosts: the file must
        // either not exist, or (if it does) still report the host as
        // unknown rather than matching/mismatching a learned entry.
        assert!(
            !path.exists() || std::fs::read_to_string(&path).unwrap().trim().is_empty(),
            "known_hosts must not be written on a fingerprint mismatch"
        );
        assert_eq!(
            check_host_key("example.com", 22, &key(KEY_B), &path),
            HostKeyCheck::Unknown,
            "a rejected key must not have been learned"
        );
    }

    #[test]
    fn no_expected_fingerprint_reports_unknown_without_learning() {
        let (_dir, path) = known_hosts_with("");
        let (key_type, fingerprint) = describe_key(&key(KEY_A));

        let resolution = resolve_unknown_host("example.com", 22, &key(KEY_A), &path, None);
        assert_eq!(
            resolution,
            UnknownHostResolution::ReportUnknown {
                key_type,
                fingerprint,
            }
        );
        assert!(
            std::fs::read_to_string(&path).unwrap().trim().is_empty(),
            "known_hosts must not be written when no fingerprint has been approved yet"
        );
    }
}
