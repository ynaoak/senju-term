//! SSH shell sessions backed by russh (pure Rust — no OpenSSL/libssh2 to
//! link, which keeps Windows/macOS/Linux builds uniform).

use std::sync::Arc;

use russh::client::{self, Handle};
use russh::keys::{load_secret_key, PrivateKeyWithHashAlg};
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

struct Client;

impl client::Handler for Client {
    type Error = russh::Error;

    // v1 trusts the server key on first use (documented in the README);
    // known-hosts verification is on the roadmap.
    async fn check_server_key(
        &mut self,
        _server_public_key: &russh::keys::PublicKey,
    ) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

impl SshSession {
    pub async fn connect(
        id: &str,
        host: &SshHost,
        secrets: SshSecrets,
        cols: u16,
        rows: u16,
        sink: Arc<dyn EventSink>,
        sessions: SessionMap,
    ) -> Result<Self, SessionError> {
        let config = Arc::new(client::Config {
            keepalive_interval: Some(std::time::Duration::from_secs(30)),
            ..Default::default()
        });

        let mut handle = client::connect(config, (host.host.as_str(), host.port), Client)
            .await
            .map_err(|e| SessionError::Ssh(format!("connect failed: {e}")))?;

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
