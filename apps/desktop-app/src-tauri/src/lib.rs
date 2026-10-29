use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use serde::{Deserialize, Serialize};
use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, State};

use senju_core::distribution::{self, DistChannel};
use senju_core::sessions::sftp;
use senju_core::session_log::{LogConfig, LoggingSink, SessionLogger};
use senju_core::keygen::{GeneratedKey, PublicKeyInfo};
use senju_core::known_hosts::KnownHostEntry;
use senju_core::sessions::{resolve_jump_chain, SessionInfo, SshSecrets, SshTestReport};
use senju_core::template;
use senju_core::devserver::{AccessLog, DevServerInfo, DevServerSink, DevServerSpec};
use senju_core::models::{DevServerPreset, HistoryEntry, HistoryMeta, SessionSnapshot};
use senju_core::{
    DevServerManager, LaunchSet, LocalSpec, Profile, SessionManager, Settings, SshHost, Stores,
    Workflow,
};

struct AppState {
    stores: Stores,
    sessions: SessionManager,
    /// Detected once at startup — see `senju_core::distribution`.
    channel: DistChannel,
    /// Kept alongside the manager so `subscribe_output` can hand the sink the
    /// webview's binary channel.
    sink: Arc<TauriSink>,
    /// Session transcripts. Wraps `sink` (see setup) so every byte a session
    /// prints can be copied to disk for the sessions that asked for it.
    logger: Arc<SessionLogger>,
    /// Default log directory when settings leave it blank.
    default_log_dir: PathBuf,
    /// Cancel flags for in-flight SFTP transfers, keyed by transfer id.
    sftp_cancels: Cancels,
    /// Built-in static servers the user has started. Runtime-only: nothing here
    /// survives a restart, because a listening socket does not either.
    dev_servers: DevServerManager,
}

/// Flush terminal output at most once every ~8ms per session (roughly a frame)
/// instead of once per PTY read, and force a flush past this size so a firehose
/// (`cat` of a big file, a build's output) can't grow an unbounded buffer.
const FLUSH_INTERVAL: Duration = Duration::from_millis(8);
const FLUSH_THRESHOLD: usize = 64 * 1024;

/// Framing for the binary output channel.
///
/// ```text
///   [0]        kind: 0 = data, 1 = exit
///   [1]        length of the session id in bytes
///   [2..2+n]   session id (UTF-8)
///   [2+n..]    output bytes, or the exit code as little-endian i32
/// ```
///
/// Data and exit share one transport on purpose: they used to be two separate
/// events, and an exit must never overtake the output that preceded it.
const FRAME_DATA: u8 = 0;
const FRAME_EXIT: u8 = 1;

fn frame(kind: u8, id: &str, payload: &[u8]) -> Option<Vec<u8>> {
    let idb = id.as_bytes();
    if idb.len() > u8::MAX as usize {
        return None; // session ids are UUIDs; anything longer is not ours
    }
    let mut out = Vec::with_capacity(2 + idb.len() + payload.len());
    out.push(kind);
    out.push(idb.len() as u8);
    out.extend_from_slice(idb);
    out.extend_from_slice(payload);
    Some(out)
}

/// Forwards session output/exit to the webview.
///
/// Reads are coalesced: instead of an emit per PTY read (thousands per second
/// during heavy output, each with full event-system overhead and a main-thread
/// decode), bytes accumulate per session and a background flusher emits one
/// batched message per session per tick.
///
/// The batch travels over a binary `Channel` once the webview has registered
/// one (`subscribe_output`): Tauri hands anything past 1KB to the webview as a
/// real ArrayBuffer, so a 64KB chunk crosses as 64KB of bytes instead of 87KB
/// of base64 that the main thread then has to decode a byte at a time. Until
/// that registration lands — the first frames of a restored session can beat
/// it — output falls back to the base64 event the frontend has always
/// understood, so nothing is dropped either way.
struct TauriSink {
    app: AppHandle,
    pending: Mutex<HashMap<String, Vec<u8>>>,
    out: Mutex<Option<Channel<InvokeResponseBody>>>,
}

#[derive(Clone, Serialize)]
struct DataEvent<'a> {
    id: &'a str,
    data: String,
}

#[derive(Clone, Serialize)]
struct ExitEvent<'a> {
    id: &'a str,
    code: i32,
}

impl TauriSink {
    fn new(app: AppHandle) -> Arc<Self> {
        let sink = Arc::new(Self {
            app,
            pending: Mutex::new(HashMap::new()),
            out: Mutex::new(None),
        });
        let flusher = sink.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(FLUSH_INTERVAL);
            flusher.flush_all();
        });
        sink
    }

    fn set_output(&self, channel: Channel<InvokeResponseBody>) {
        *self.out.lock().unwrap_or_else(|e| e.into_inner()) = Some(channel);
    }

    /// Sends one framed message over the binary channel. Returns false when no
    /// channel is registered yet or the webview has gone away, so the caller
    /// can fall back to the event path.
    fn send_frame(&self, kind: u8, id: &str, payload: &[u8]) -> bool {
        let guard = self.out.lock().unwrap_or_else(|e| e.into_inner());
        let Some(channel) = guard.as_ref() else {
            return false;
        };
        let Some(bytes) = frame(kind, id, payload) else {
            return false;
        };
        channel.send(InvokeResponseBody::Raw(bytes)).is_ok()
    }

    fn emit_data(&self, id: &str, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        if self.send_frame(FRAME_DATA, id, bytes) {
            return;
        }
        let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
        // Deliver only to the main webview, not every window — terminal output
        // (which may include typed passwords echoed by a remote host) must not
        // fan out to any future auxiliary window.
        let _ = self
            .app
            .emit_to("main", "session:data", DataEvent { id, data: encoded });
    }

    fn flush_all(&self) {
        let drained: Vec<(String, Vec<u8>)> = {
            let mut map = self.pending.lock().unwrap_or_else(|e| e.into_inner());
            map.drain().filter(|(_, v)| !v.is_empty()).collect()
        };
        for (id, bytes) in drained {
            self.emit_data(&id, &bytes);
        }
    }

    /// Flush a single session's buffer immediately (on threshold, and before
    /// its exit event so output always precedes exit).
    fn flush_one(&self, id: &str) {
        let bytes = {
            let mut map = self.pending.lock().unwrap_or_else(|e| e.into_inner());
            map.remove(id)
        };
        if let Some(bytes) = bytes {
            self.emit_data(id, &bytes);
        }
    }
}

impl senju_core::EventSink for TauriSink {
    fn data(&self, id: &str, data: &[u8]) {
        let over_threshold = {
            let mut map = self.pending.lock().unwrap_or_else(|e| e.into_inner());
            let buf = map.entry(id.to_string()).or_default();
            buf.extend_from_slice(data);
            buf.len() >= FLUSH_THRESHOLD
        };
        if over_threshold {
            self.flush_one(id);
        }
    }

    fn exit(&self, id: &str, code: i32) {
        self.flush_one(id); // any buffered output must land before the exit
        if self.send_frame(FRAME_EXIT, id, &code.to_le_bytes()) {
            return;
        }
        let _ = self
            .app
            .emit_to("main", "session:exit", ExitEvent { id, code });
    }
}

type CmdResult<T> = Result<T, String>;

// -- Sessions -----------------------------------------------------------------

/// Registers the webview's binary channel for terminal output. Called once,
/// first thing at boot: every session created afterwards streams over it, and
/// the sink falls back to the base64 event for anything emitted before this
/// lands.
#[tauri::command]
fn subscribe_output(channel: Channel<InvokeResponseBody>, state: State<'_, AppState>) {
    state.sink.set_output(channel);
}

#[tauri::command]
fn create_local_session(
    state: State<AppState>,
    profile_id: Option<String>,
    cols: u16,
    rows: u16,
) -> CmdResult<SessionStarted> {
    // Resolve the requested profile (or the configured default). Fall back to
    // the legacy `settings.shell` override when no profiles exist at all.
    let spec = match state.stores.resolve_profile(profile_id.as_deref()) {
        Some(p) => LocalSpec {
            command: p.command,
            args: p.args,
            cwd: p.cwd,
            title: p.name,
        },
        None => LocalSpec {
            command: state.stores.settings().shell,
            ..Default::default()
        },
    };
    let settings = state.stores.settings();
    let info = state
        .sessions
        .create_local(&spec, cols, rows, settings.shell_integration)
        .map_err(|e| e.to_string())?;
    Ok(start_log(&state, info, settings.session_log_local))
}

/// What the webview gets back for a new session: the session, plus where its
/// transcript is being written (or why it is not, when logging was asked for
/// and the file could not be opened — the session still runs; the UI just
/// says so).
#[derive(Serialize)]
struct SessionStarted {
    #[serde(flatten)]
    info: SessionInfo,
    log_path: Option<String>,
    log_error: Option<String>,
}

fn start_log(state: &AppState, info: SessionInfo, wanted: bool) -> SessionStarted {
    let (log_path, log_error) = if wanted {
        match state.logger.start(&info.id, &info.title) {
            Ok(p) => (Some(p.to_string_lossy().into_owned()), None),
            Err(e) => (None, Some(e.to_string())),
        }
    } else {
        (None, None)
    };
    SessionStarted { info, log_path, log_error }
}

/// Resolves the configured log directory: blank means the app's own
/// `logs` folder, `~` expands.
fn log_config(default_dir: &Path, settings: &Settings) -> LogConfig {
    let dir = settings.session_log_dir.trim();
    let dir = if dir.is_empty() { default_dir.to_path_buf() } else { PathBuf::from(expand_home(dir)) };
    LogConfig { dir, plain: settings.session_log_plain }
}

/* ---------------- known_hosts / key pairs ---------------- */

fn known_hosts_file() -> CmdResult<PathBuf> {
    senju_core::known_hosts::default_path().ok_or_else(|| "ホームディレクトリが分かりません".into())
}

/// Every entry of `~/.ssh/known_hosts`, for the management dialog.
#[tauri::command]
fn list_known_hosts() -> CmdResult<Vec<KnownHostEntry>> {
    senju_core::known_hosts::list(&known_hosts_file()?).map_err(|e| e.to_string())
}

/// Removes the given 1-based lines from known_hosts. The next connection to
/// that host goes through the TOFU prompt again, as a first contact.
#[tauri::command]
fn remove_known_hosts(lines: Vec<usize>) -> CmdResult<usize> {
    senju_core::known_hosts::remove_lines(&known_hosts_file()?, &lines).map_err(|e| e.to_string())
}

fn ssh_dir() -> CmdResult<PathBuf> {
    senju_core::keygen::default_dir().ok_or_else(|| "ホームディレクトリが分かりません".into())
}

/// Generates an Ed25519 pair in `~/.ssh`. The passphrase, when given, is
/// used once here and never stored.
#[tauri::command]
fn generate_ssh_key(name: String, comment: String, passphrase: Option<String>) -> CmdResult<GeneratedKey> {
    senju_core::keygen::generate_ed25519(&ssh_dir()?, &name, &comment, passphrase.as_deref())
}

#[tauri::command]
fn list_public_keys() -> CmdResult<Vec<PublicKeyInfo>> {
    Ok(senju_core::keygen::list_public_keys(&ssh_dir()?))
}

/// Appends the public key at `pub_path` to the remote user's
/// `authorized_keys` over the given SSH session (ssh-copy-id). Returns true
/// when the key was added, false when it was already there.
#[tauri::command]
async fn install_public_key(state: State<'_, AppState>, session_id: String, pub_path: String) -> CmdResult<bool> {
    let path = PathBuf::from(expand_home(&pub_path));
    let line = std::fs::read_to_string(&path).map_err(|e| format!("{} を読めません: {e}", path.display()))?;
    senju_core::keygen::validate_public_key_line(&line)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    state
        .sessions
        .with_sftp(&session_id, |sftp| async move {
            senju_core::sessions::sftp::install_authorized_key(&sftp, line.trim()).await
        })
        .await
        .map_err(|e| e.to_string())
}

/// Where session logs are written right now, for the settings panel's
/// "open folder" button and placeholder.
#[tauri::command]
fn session_log_dir(state: State<AppState>) -> String {
    let dir = state.logger.config().dir;
    // Created here so "open folder" works before the first log exists;
    // failure is not an error — the folder is only missing until it isn't.
    let _ = std::fs::create_dir_all(&dir);
    dir.to_string_lossy().into_owned()
}

/// Per-hop secret sent from the UI, keyed by saved-host id. Only the fields a
/// given auth method needs are populated; both being `None` is fine (agent).
#[derive(Deserialize, Default)]
struct HopSecret {
    #[serde(default)]
    password: Option<String>,
    #[serde(default)]
    passphrase: Option<String>,
}

#[tauri::command]
async fn create_ssh_session(
    state: State<'_, AppState>,
    host_id: String,
    // Per-hop credentials keyed by host id (the target plus any jump hosts in
    // the ProxyJump chain). Never persisted.
    secrets: HashMap<String, HopSecret>,
    // Per-hop approved SHA256 fingerprints keyed by host id. A key here means:
    // that hop was unknown, the UI already showed the user this fingerprint
    // (from a previous `UNKNOWN_HOST_KEY` failure) and the user approved it.
    // The backend only trusts and records a hop's key if the one presented on
    // THIS handshake has that exact fingerprint — see `sessions::ssh` for why
    // a plain `trust_host: bool` was unsafe (TOFU TOCTOU). Keying by host id
    // means each hop in a multi-hop chain is approved independently.
    fingerprints: HashMap<String, String>,
    cols: u16,
    rows: u16,
) -> CmdResult<SessionStarted> {
    let all_hosts = state.stores.list_ssh_hosts();
    let target = all_hosts
        .iter()
        .find(|h| h.id == host_id)
        .ok_or_else(|| format!("unknown host: {host_id}"))?
        .clone();
    // Resolve (and validate) the jump chain here — the store is the source of
    // truth for what each jump-host id points at.
    let chain = resolve_jump_chain(&target, &all_hosts)?;
    let mut secrets_by_id: HashMap<String, SshSecrets> = secrets
        .into_iter()
        .map(|(id, s)| {
            (
                id,
                SshSecrets {
                    password: s.password,
                    passphrase: s.passphrase,
                },
            )
        })
        .collect();

    // Hops the user typed a secret for on THIS connect. Only these are
    // candidates for saving afterwards: anything that came out of the
    // credential store is already there, and rewriting it would turn a failed
    // read into a silent overwrite.
    let typed: Vec<String> = secrets_by_id.keys().cloned().collect();

    // Fill the gaps from what each host has saved. `chain` comes from the
    // store, not the webview, so these are the real values — the frontend is
    // told only *whether* one exists (`ssh_saved_credentials`).
    for hop in &chain {
        if !hop.save_credentials || secrets_by_id.contains_key(&hop.id) {
            continue;
        }
        let stored = SshSecrets {
            password: Some(hop.password.clone()).filter(|p| !p.is_empty()),
            passphrase: Some(hop.passphrase.clone()).filter(|p| !p.is_empty()),
        };
        if stored.password.is_some() || stored.passphrase.is_some() {
            secrets_by_id.insert(hop.id.clone(), stored);
        }
    }

    let info = state
        .sessions
        .create_ssh_chain(&chain, &secrets_by_id, cols, rows, &fingerprints)
        .await
        .map_err(|e| e.to_string())?;

    // Save only after the connection succeeded. Storing what the user typed
    // before knowing it works would persist a typo and then keep replaying
    // it, which looks like the server rejecting a correct password.
    for id in typed {
        let Some(host) = chain.iter().find(|h| h.id == id && h.save_credentials) else {
            continue;
        };
        let Some(s) = secrets_by_id.get(&id) else { continue };
        // A blank field erases rather than keeps: a `keypassword` host whose
        // passphrase turned out to be unnecessary should not leave the old one
        // behind. Failures are swallowed — the session is already up, and
        // failing a working connection over a write would be the wrong trade.
        let updated = SshHost {
            password: s.password.clone().unwrap_or_default(),
            passphrase: s.passphrase.clone().unwrap_or_default(),
            ..host.clone()
        };
        let _ = state.stores.save_ssh_host(updated);
    }
    // The target is the last hop; its own name labels the file, not the
    // bastion's.
    let target = chain.iter().find(|h| h.id == host_id);
    let wanted = target.is_some_and(|h| h.log_sessions);
    let label = target.map(|h| if h.name.trim().is_empty() { h.host.clone() } else { h.name.clone() });
    let info = match label {
        Some(name) if wanted => SessionInfo { title: name, ..info.clone() },
        _ => info,
    };
    Ok(start_log(&state, info, wanted))
}

/// Whether a saved credential exists for each of the given hosts, so the
/// frontend knows which hops still need a prompt. Returns only booleans —
/// the secrets themselves never enter the webview.
#[tauri::command]
fn ssh_saved_credentials(state: State<AppState>, host_ids: Vec<String>) -> HashMap<String, bool> {
    let hosts = state.stores.list_ssh_hosts();
    host_ids
        .into_iter()
        .map(|id| {
            let saved = hosts
                .iter()
                .find(|h| h.id == id)
                .is_some_and(|h| !h.password.is_empty() || !h.passphrase.is_empty());
            (id, saved)
        })
        .collect()
}

/// Forgets one host's saved credentials, for the "この接続の保存済み認証情報を
/// 削除" action in the host editor.
#[tauri::command]
fn forget_ssh_credentials(state: State<AppState>, host_id: String) -> CmdResult<()> {
    let Some(host) = state.stores.list_ssh_hosts().into_iter().find(|h| h.id == host_id) else {
        return Ok(()); // already gone; nothing to withdraw
    };
    state
        .stores
        .save_ssh_host(SshHost { password: String::new(), passphrase: String::new(), ..host })
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Pre-save connection probe for the SSH host editor. Takes the host fields
/// directly (the host may not be saved yet) plus test-only secrets, and
/// verifies reachability + credentials without opening a persistent session
/// or recording the host key.
#[tauri::command]
async fn test_ssh_connection(
    state: State<'_, AppState>,
    host: SshHost,
    password: Option<String>,
    passphrase: Option<String>,
    // Same TOFU handshake as `create_ssh_session`: `None` first contact returns
    // UNKNOWN_HOST_KEY (before any credential is sent); the UI re-tests with the
    // approved fingerprint here.
    expected_fingerprint: Option<String>,
) -> CmdResult<SshTestReport> {
    state
        .sessions
        .test_ssh(
            &host,
            SshSecrets { password, passphrase },
            expected_fingerprint,
        )
        .await
        .map_err(|e| e.to_string())
}

/// Opens a link (e.g. one xterm detected in terminal output) in the OS default
/// browser. Terminal output is attacker-controlled, so this hard-restricts the
/// scheme to http/https — never `file:`, `javascript:`, custom protocol
/// handlers, etc. — and hands the URL to the `open` crate, which uses
/// ShellExecute/open(1)/xdg-open rather than a shell, so URL contents can't be
/// reinterpreted as a command.
#[tauri::command]
fn open_external(url: String) -> CmdResult<()> {
    let lower = url.to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return Err("http(s) 以外の URL は開けません".into());
    }
    open::that_detached(&url).map_err(|e| e.to_string())
}

/* ---------------- SFTP file transfer ---------------- */

/// One transfer's progress, emitted to the frontend as `sftp:progress`.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SftpProgress<'a> {
    transfer_id: &'a str,
    transferred: u64,
    /// 0 when the size is unknown (a server that refused `stat`), which the
    /// UI shows as an indeterminate bar rather than a bogus percentage.
    total: u64,
}

/// Result of a finished transfer.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SftpDone {
    bytes: u64,
    /// The path actually used on the server, after `~` and relative paths
    /// were resolved there. Shown back to the user because "where did it go"
    /// is the first question after an upload.
    remote_path: String,
}

/// Flags for in-flight transfers, keyed by the id the frontend generated.
/// `sftp_cancel` flips one; the transfer loop checks it between chunks.
type Cancels = Arc<Mutex<HashMap<String, Arc<std::sync::atomic::AtomicBool>>>>;

/// Registers a cancel flag for `transfer_id` and returns it plus a guard that
/// removes the entry however the transfer ends — including on an early
/// return, which is what stops the map growing a dead flag per failure.
fn cancel_slot(cancels: &Cancels, transfer_id: &str) -> Arc<std::sync::atomic::AtomicBool> {
    let flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
    cancels
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(transfer_id.to_string(), flag.clone());
    flag
}

fn cancel_release(cancels: &Cancels, transfer_id: &str) {
    cancels
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(transfer_id);
}

/// Emits progress to the main webview only — the same restriction the
/// terminal data path uses, since a transfer's paths are as much the user's
/// business as their terminal output.
fn progress_emitter<'a>(
    app: &'a AppHandle,
    transfer_id: &'a str,
) -> impl Fn(u64, u64) + Send + Sync + 'a {
    move |transferred, total| {
        let _ = app.emit_to(
            "main",
            "sftp:progress",
            SftpProgress { transfer_id, transferred, total },
        );
    }
}

/// Uploads a local file to `remote_path` on the session's host, over the SSH
/// connection that session already holds.
#[tauri::command]
async fn sftp_upload(
    app: AppHandle,
    state: State<'_, AppState>,
    session_id: String,
    local_path: String,
    remote_path: String,
    transfer_id: String,
) -> CmdResult<SftpDone> {
    let cancels = state.sftp_cancels.clone();
    let flag = cancel_slot(&cancels, &transfer_id);
    let tid = transfer_id.clone();
    let result = state
        .sessions
        .with_sftp(&session_id, |sftp| async move {
            // Resolve on the server so `~/dumps/x.sql` and a relative path
            // land where the user's shell would put them, and so the path
            // reported back is the real one.
            let resolved = sftp::canonicalize(&sftp, &remote_path).await.unwrap_or(remote_path);
            let bytes = sftp::upload(
                &sftp,
                Path::new(&local_path),
                &resolved,
                &flag,
                &progress_emitter(&app, &tid),
            )
            .await?;
            Ok(SftpDone { bytes, remote_path: resolved })
        })
        .await;
    cancel_release(&cancels, &transfer_id);
    result.map_err(|e| e.to_string())
}

/// Uploads a local file or folder (a path the OS handed us from a drop or an
/// open dialog) *into* `remote_dir`, keeping its own name. A folder is
/// recreated recursively; progress counts toward the whole tree. `remote_dir`
/// is resolved on the server first (`~`, relative), and the path reported
/// back is the one actually created.
#[tauri::command]
async fn sftp_upload_into(
    app: AppHandle,
    state: State<'_, AppState>,
    session_id: String,
    local_path: String,
    remote_dir: String,
    transfer_id: String,
) -> CmdResult<SftpDone> {
    let cancels = state.sftp_cancels.clone();
    let flag = cancel_slot(&cancels, &transfer_id);
    let tid = transfer_id.clone();
    let result = state
        .sessions
        .with_sftp(&session_id, |sftp| async move {
            let dir = if remote_dir.trim().is_empty() { ".".to_string() } else { remote_dir };
            let resolved = sftp::canonicalize(&sftp, &dir).await.unwrap_or(dir);
            let (remote_path, bytes) = sftp::upload_into(
                &sftp,
                Path::new(&local_path),
                &resolved,
                &flag,
                &progress_emitter(&app, &tid),
            )
            .await?;
            Ok(SftpDone { bytes, remote_path })
        })
        .await;
    cancel_release(&cancels, &transfer_id);
    result.map_err(|e| e.to_string())
}

/// Downloads `remote_path` from the session's host into a local file.
#[tauri::command]
async fn sftp_download(
    app: AppHandle,
    state: State<'_, AppState>,
    session_id: String,
    remote_path: String,
    local_path: String,
    transfer_id: String,
) -> CmdResult<SftpDone> {
    let cancels = state.sftp_cancels.clone();
    let flag = cancel_slot(&cancels, &transfer_id);
    let tid = transfer_id.clone();
    let result = state
        .sessions
        .with_sftp(&session_id, |sftp| async move {
            let resolved = sftp::canonicalize(&sftp, &remote_path).await.unwrap_or(remote_path);
            let bytes = sftp::download(
                &sftp,
                &resolved,
                Path::new(&local_path),
                &flag,
                &progress_emitter(&app, &tid),
            )
            .await?;
            Ok(SftpDone { bytes, remote_path: resolved })
        })
        .await;
    cancel_release(&cancels, &transfer_id);
    result.map_err(|e| e.to_string())
}

/// A remote directory listing plus the path it resolved to.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RemoteListing {
    /// The absolute path the server resolved the request to. The browser
    /// navigates by this rather than by what the user typed, so `~` and `..`
    /// never accumulate in the displayed path.
    path: String,
    /// The parent directory, or `None` at the filesystem root — which is what
    /// the UI needs to know whether to offer "up".
    parent: Option<String>,
    entries: Vec<sftp::RemoteEntry>,
}

/// Lists a directory on the session's host, for the remote file browser.
#[tauri::command]
async fn sftp_list(
    state: State<'_, AppState>,
    session_id: String,
    path: String,
) -> CmdResult<RemoteListing> {
    state
        .sessions
        .with_sftp(&session_id, |sftp| async move {
            let (resolved, entries) = sftp::read_dir(&sftp, &path).await?;
            Ok(RemoteListing { parent: parent_path(&resolved), path: resolved, entries })
        })
        .await
        .map_err(|e| e.to_string())
}

/// The parent of an absolute POSIX path, or `None` for `/` itself.
///
/// Computed here rather than in the frontend so the "up" target comes from
/// the same string the server resolved — a path split in JS would have to
/// re-derive assumptions about separators that SFTP already settled (it is
/// always `/`, whatever the server's OS calls things elsewhere).
fn parent_path(path: &str) -> Option<String> {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return None; // already at "/"
    }
    match trimmed.rfind('/') {
        Some(0) => Some("/".to_string()),
        Some(i) => Some(trimmed[..i].to_string()),
        None => None,
    }
}

/// Asks a running transfer to stop. Unknown ids are a no-op: the transfer may
/// have finished between the user clicking and this arriving, which is not an
/// error worth reporting.
#[tauri::command]
fn sftp_cancel(state: State<'_, AppState>, transfer_id: String) {
    if let Some(flag) = state
        .sftp_cancels
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&transfer_id)
    {
        flag.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Shows a path in the OS file manager, selecting it in its containing folder.
///
/// This deliberately **reveals** rather than opens. Handing a path from
/// terminal output to `open::that` would launch it with whatever application
/// claims that extension — a `.desktop`, `.command`, `.exe` or a registered
/// custom handler is then arbitrary code execution triggered by a line a
/// remote host printed. Revealing shows the file in a file manager and
/// nothing runs.
///
/// The path must be absolute (`~` is expanded here rather than in the
/// frontend, which does not know this machine's home) and must exist: a
/// reveal of something that is not there tends to open the wrong folder, or
/// the file manager's idea of "nearest existing ancestor", which is worse
/// than saying no. No shell is involved — the path is one argument to a
/// fixed program.
#[tauri::command]
fn reveal_path(path: String) -> CmdResult<()> {
    let expanded = expand_home(&path);
    let p = Path::new(&expanded);
    if !p.is_absolute() {
        return Err("絶対パスのみ開けます".into());
    }
    // Resolving symlinks also collapses any `..`, so what gets revealed is
    // the file the user is looking at rather than a path that merely spells
    // its way there.
    let real = std::fs::canonicalize(p).map_err(|_| format!("パスが見つかりません: {path}"))?;
    reveal_in_file_manager(&real).map_err(|e| e.to_string())
}

/// Opens a terminal emulator outside the app, in `path` (or, for a file, its
/// parent directory).
///
/// Which terminal is the `external_terminal` setting; empty means the OS
/// default. Same path rules as `reveal_path`: absolute (after `~` expansion)
/// and existing. No shell is involved, and the child is never waited on.
#[tauri::command]
fn open_external_terminal(state: State<'_, AppState>, path: String) -> CmdResult<()> {
    let expanded = expand_home(&path);
    let p = Path::new(&expanded);
    if !p.is_absolute() {
        return Err("絶対パスのみ開けます".into());
    }
    let real = std::fs::canonicalize(p).map_err(|_| format!("パスが見つかりません: {path}"))?;
    let dir = if real.is_dir() {
        real
    } else {
        real.parent().map(Path::to_path_buf).unwrap_or(real)
    };
    let dir = strip_verbatim_prefix(&dir);

    let setting = state.stores.settings().external_terminal;
    let mut words = senju_core::extterm::split_command_line(setting.trim());
    if words.is_empty() {
        return open_default_terminal(&dir).map_err(|e| format!("外部ターミナルを起動できません: {e}"));
    }
    let dir_str = dir.display().to_string();
    for w in &mut words {
        *w = w.replace("{dir}", &dir_str);
    }
    let prog = words.remove(0);
    let mut cmd = std::process::Command::new(&prog);
    cmd.args(&words).current_dir(&dir);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NEW_CONSOLE: console programs (cmd.exe, pwsh.exe) get their own window.
        cmd.creation_flags(0x0000_0010);
    }
    cmd.spawn()
        .map(|_| ())
        .map_err(|e| format!("外部ターミナルを起動できません ({prog}): {e}"))
}

/// Drops the `\\?\` prefix `canonicalize` adds on Windows, which most
/// programs (Explorer, terminals) do not understand. A no-op elsewhere.
/// A network share comes back as `\\?\UNC\server\share`, which has to turn
/// into `\\server\share` rather than lose its leading slashes.
fn strip_verbatim_prefix(path: &Path) -> std::path::PathBuf {
    let display = path.display().to_string();
    if let Some(rest) = display.strip_prefix(r#"\\?\UNC\"#) {
        return std::path::PathBuf::from(format!(r"\\{rest}"));
    }
    match display.strip_prefix(r#"\\?\"#) {
        Some(clean) => std::path::PathBuf::from(clean),
        None => path.to_path_buf(),
    }
}

#[cfg(target_os = "windows")]
fn open_default_terminal(dir: &Path) -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;
    // Windows has no "launch the default terminal" API. What it has is the
    // default terminal application setting (Windows 11): a console program
    // started with a new console is handed to whichever terminal the user
    // chose there, Windows Terminal or conhost. So start the command
    // interpreter with CREATE_NEW_CONSOLE and let Windows route it.
    let comspec = std::env::var("ComSpec").unwrap_or_else(|_| "cmd.exe".into());
    std::process::Command::new(comspec)
        .current_dir(dir)
        .creation_flags(0x0000_0010)
        .spawn()
        .map(|_| ())
}

#[cfg(target_os = "macos")]
fn open_default_terminal(dir: &Path) -> std::io::Result<()> {
    std::process::Command::new("open")
        .arg("-a")
        .arg("Terminal")
        .arg(dir)
        .spawn()
        .map(|_| ())
}

#[cfg(all(unix, not(target_os = "macos")))]
fn open_default_terminal(dir: &Path) -> std::io::Result<()> {
    let mut candidates: Vec<String> = Vec::new();
    if let Ok(t) = std::env::var("TERMINAL") {
        if !t.trim().is_empty() {
            candidates.push(t);
        }
    }
    candidates.extend(
        ["x-terminal-emulator", "gnome-terminal", "konsole", "xfce4-terminal", "xterm"]
            .map(String::from),
    );
    for c in candidates {
        // Only the spawn matters; the terminal runs on its own.
        if std::process::Command::new(&c).current_dir(dir).spawn().is_ok() {
            return Ok(());
        }
    }
    Err(std::io::Error::new(std::io::ErrorKind::NotFound, "no terminal emulator found"))
}

/// Lists the runnable tasks declared in `dir` (`package.json` scripts, make
/// targets, and the rest — see `senju_core::tasks`).
///
/// `dir` is a shell's reported working directory, so it is taken as given
/// rather than validated against anything: a directory that has gone away, or
/// that holds nothing recognizable, is an empty list and not an error. The
/// panel that calls this runs on every `cd`, and an error toast per directory
/// without a `package.json` would be unusable.
///
/// Nothing is executed here. The commands come back as text for the user to
/// press, which keeps the decision to run something in the UI where the user
/// can see what they are about to run.
#[tauri::command]
fn scan_tasks(dir: String) -> Vec<senju_core::tasks::TaskGroup> {
    let expanded = expand_home(&dir);
    let path = Path::new(&expanded);
    if !path.is_absolute() {
        return Vec::new();
    }
    senju_core::tasks::scan(path)
}

/* ---------------- built-in dev server ---------------- */

/// Forwards the server's activity to the webview.
///
/// Access lines are emitted as they happen rather than buffered and polled: a
/// preview server sees a handful of requests per page load, and a log that
/// appears only when the panel asks would miss exactly the request the user is
/// trying to explain.
struct DevServerEvents {
    app: AppHandle,
}

impl DevServerSink for DevServerEvents {
    fn access(&self, log: AccessLog) {
        let _ = self.app.emit("devserver:access", log);
    }

    fn reloaded(&self, server_id: &str, path: String) {
        let _ = self.app.emit("devserver:reloaded", (server_id, path));
    }

    fn watch_failed(&self, server_id: &str, message: String) {
        let _ = self.app.emit("devserver:watch-failed", (server_id, message));
    }

    fn watch_limited(&self, server_id: &str, watched: usize) {
        let _ = self.app.emit("devserver:watch-limited", (server_id, watched));
    }
}

/// Starts a server for `root` on `port` (0 = any free port).
///
/// Returns only once the socket is listening, so a port clash surfaces as an
/// error on the button rather than a server that silently never came up.
#[tauri::command]
async fn start_dev_server(
    state: State<'_, AppState>,
    root: String,
    port: u16,
    spa: bool,
    hot_reload: bool,
) -> CmdResult<DevServerInfo> {
    let root = expand_home(&root);
    if !Path::new(&root).is_absolute() {
        return Err("公開するディレクトリは絶対パスで指定してください".into());
    }
    state
        .dev_servers
        .start(DevServerSpec { root: root.clone(), port, spa, hot_reload })
        .await
        .map_err(|e| match e {
            senju_core::devserver::DevServerError::NotADirectory(d) => {
                format!("ディレクトリが見つかりません: {d}")
            }
            senju_core::devserver::DevServerError::PortInUse(p) => {
                format!("ポート {p} は既に使われています。別のポートを指定するか、0 で空きポートを自動割り当てしてください")
            }
            other => other.to_string(),
        })
}

#[tauri::command]
fn stop_dev_server(state: State<'_, AppState>, id: String) -> bool {
    state.dev_servers.stop(&id)
}

#[tauri::command]
fn list_dev_servers(state: State<'_, AppState>) -> Vec<DevServerInfo> {
    state.dev_servers.list()
}

#[tauri::command]
fn list_dev_server_presets(state: State<'_, AppState>) -> Vec<DevServerPreset> {
    state.stores.list_dev_servers()
}

#[tauri::command]
fn save_dev_server_preset(
    state: State<'_, AppState>,
    preset: DevServerPreset,
) -> CmdResult<DevServerPreset> {
    state.stores.save_dev_server(preset).map_err(|e| e.to_string())
}

#[tauri::command]
fn delete_dev_server_preset(state: State<'_, AppState>, id: String) -> CmdResult<()> {
    state.stores.delete_dev_server(&id).map_err(|e| e.to_string())
}

/// Expands a leading `~` / `~/` to the user's home. Anything else — including
/// `~user` — is left alone: resolving another user's home needs a passwd
/// lookup we do not do, and guessing would reveal the wrong directory.
fn expand_home(path: &str) -> String {
    let Some(rest) = path.strip_prefix('~') else {
        return path.to_string();
    };
    if !(rest.is_empty() || rest.starts_with('/') || rest.starts_with('\\')) {
        return path.to_string();
    }
    match dirs::home_dir() {
        Some(home) => format!("{}{}", home.display(), rest),
        None => path.to_string(),
    }
}

#[cfg(target_os = "windows")]
fn reveal_in_file_manager(path: &Path) -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;
    // `/select,PATH` has to reach Explorer as ONE argument with the comma
    // unquoted, which `Command::arg` cannot express — it would quote the
    // whole thing once the path contains a space. `raw_arg` passes the
    // command line through verbatim, so the quotes go where Explorer wants
    // them. Canonicalize gives a `\?\` prefix that Explorer does not
    // understand, so strip it.
    let display = path.display().to_string();
    let clean = display.strip_prefix(r#"\\?\"#).unwrap_or(&display);
    std::process::Command::new("explorer.exe")
        .raw_arg(format!("/select,\"{clean}\""))
        .spawn()
        // Explorer exits non-zero even when it worked, so only the spawn is
        // checked; waiting on the status would report a failure that isn't.
        .map(|_| ())
}

#[cfg(target_os = "macos")]
fn reveal_in_file_manager(path: &Path) -> std::io::Result<()> {
    std::process::Command::new("open")
        .arg("-R")
        .arg(path)
        .spawn()
        .map(|_| ())
}

#[cfg(all(unix, not(target_os = "macos")))]
fn reveal_in_file_manager(path: &Path) -> std::io::Result<()> {
    // No portable "reveal" on Linux: file managers disagree, and the
    // freedesktop `ShowItems` D-Bus call is not always answered. Opening the
    // containing directory works with every `xdg-open` handler, at the cost
    // of not highlighting the file itself. A directory opens as itself.
    let target = if path.is_dir() {
        path
    } else {
        path.parent().unwrap_or(path)
    };
    std::process::Command::new("xdg-open")
        .arg(target)
        .spawn()
        .map(|_| ())
}

/* ---------------- distribution channel ---------------- */

/// What the frontend needs to decide who updates this copy of the app.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DistInfo {
    /// Stable channel id — `direct-nsis`, `msstore`, `system-package`, …
    channel: &'static str,
    /// The installer this copy came from, when known (`nsis`, `msi`, `app`, …).
    installer: Option<&'static str>,
    /// Whether the in-app updater is allowed to run at all. When false the
    /// updater plugin is not even registered, so `check()` would fail anyway.
    in_app_updates: bool,
    /// True when *some other* app applies updates and we can hand off to it.
    has_external_manager: bool,
}

/// Reports how this copy was installed. The frontend uses it to run, hide, or
/// redirect the update flow — see `senju_core::distribution` for why each
/// channel has to be treated differently.
#[tauri::command]
fn dist_info(state: State<'_, AppState>) -> DistInfo {
    let c = state.channel;
    DistInfo {
        channel: c.id(),
        installer: c.installer_format().map(|f| f.id()),
        in_app_updates: c.in_app_updates(),
        has_external_manager: c.external_update_target().is_some(),
    }
}

/// Hands off to whatever actually owns updates for this build — the Microsoft
/// Store's or the App Store's updates pane.
///
/// Unlike `open_external` this deliberately accepts no URL from the frontend:
/// the only reachable targets are the two fixed store URIs baked into
/// `DistChannel`, so the non-http scheme cannot be pointed anywhere else.
#[tauri::command]
fn open_update_manager(state: State<'_, AppState>) -> CmdResult<()> {
    let target = state
        .channel
        .external_update_target()
        .ok_or("このビルドには外部のアップデート管理がありません")?;
    open::that_detached(target).map_err(|e| e.to_string())
}

#[tauri::command]
async fn session_write(state: State<'_, AppState>, id: String, data: String) -> CmdResult<()> {
    state
        .sessions
        .write(&id, data.as_bytes())
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn session_resize(
    state: State<'_, AppState>,
    id: String,
    cols: u16,
    rows: u16,
) -> CmdResult<()> {
    state
        .sessions
        .resize(&id, cols, rows)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn session_kill(state: State<AppState>, id: String) {
    state.sessions.kill(&id);
}

// -- Workflows ------------------------------------------------------------------

#[tauri::command]
fn list_workflows(state: State<AppState>) -> Vec<Workflow> {
    state.stores.list_workflows()
}

#[tauri::command]
fn save_workflow(state: State<AppState>, workflow: Workflow) -> CmdResult<Workflow> {
    state
        .stores
        .save_workflow(workflow)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn delete_workflow(state: State<AppState>, id: String) -> CmdResult<()> {
    state.stores.delete_workflow(&id).map_err(|e| e.to_string())
}

/// Last values typed into a workflow's placeholder prompt, for its defaults.
#[tauri::command]
fn get_workflow_inputs(state: State<AppState>, id: String) -> HashMap<String, String> {
    state.stores.workflow_inputs(&id)
}

/// Remembers (or, with an empty map, forgets) a workflow's placeholder values.
#[tauri::command]
fn save_workflow_inputs(state: State<AppState>, id: String, values: HashMap<String, String>) -> CmdResult<()> {
    state.stores.save_workflow_inputs(&id, values).map_err(|e| e.to_string())
}

#[tauri::command]
fn reorder_workflows(state: State<AppState>, ids: Vec<String>) -> CmdResult<()> {
    state
        .stores
        .reorder_workflows(&ids)
        .map_err(|e| e.to_string())
}

/// Writes every stored workflow to `path` as a shareable JSON envelope
/// (ids stripped). Returns how many were exported. The path comes from a
/// native save dialog on the frontend.
#[tauri::command]
fn export_workflows(state: State<AppState>, path: String) -> CmdResult<usize> {
    let workflows = state.stores.list_workflows();
    std::fs::write(&path, senju_core::share::to_export_json(&workflows))
        .map_err(|e| e.to_string())?;
    Ok(workflows.len())
}

/// Parses a workflow-export file into import candidates. Nothing is saved —
/// the frontend shows a checklist and saves each picked workflow through the
/// normal save_workflow path (fresh ids).
#[tauri::command]
fn read_workflows_file(path: String) -> CmdResult<Vec<Workflow>> {
    let content = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    senju_core::share::from_export_json(&content)
}

#[tauri::command]
fn workflow_placeholders(command: String) -> Vec<template::Placeholder> {
    template::extract_placeholders(&command)
}

/// The `$`-built-ins a template uses, so the UI resolves only those (and
/// touches the clipboard only for a template that names it).
#[tauri::command]
fn workflow_builtins(command: String) -> Vec<String> {
    template::builtin_names(&command)
}

#[tauri::command]
fn fill_workflow(command: String, values: HashMap<String, String>) -> String {
    template::fill_placeholders(&command, &values)
}

// -- SSH hosts --------------------------------------------------------------------

#[tauri::command]
fn list_ssh_hosts(state: State<AppState>) -> Vec<SshHost> {
    state.stores.list_ssh_hosts().into_iter().map(redact_host).collect()
}

/// Strips a host's saved password/passphrase before it crosses into the
/// webview. They are stored in a file now rather than an OS credential store,
/// but the rule that the values themselves never reach the frontend has not
/// changed: the editor is told only *whether* something is saved
/// (`save_credentials`), which is all it needs to decide whether to prompt.
fn redact_host(mut host: SshHost) -> SshHost {
    host.password = String::new();
    host.passphrase = String::new();
    host
}

/// Saves a host. A blank password/passphrase means "leave the saved one
/// alone", not "erase it" — the editor never received them (see
/// `redact_host`), so every ordinary edit sends blanks back, and treating that
/// as a delete would drop the credentials whenever someone renamed a host.
/// Unticking `save_credentials` is the explicit erase, handled in the store.
#[tauri::command]
fn save_ssh_host(state: State<AppState>, mut host: SshHost) -> CmdResult<SshHost> {
    if host.save_credentials && !host.id.is_empty() {
        if let Some(existing) = state.stores.list_ssh_hosts().into_iter().find(|h| h.id == host.id) {
            if host.password.is_empty() {
                host.password = existing.password;
            }
            if host.passphrase.is_empty() {
                host.passphrase = existing.passphrase;
            }
        }
    }
    state.stores.save_ssh_host(host).map(redact_host).map_err(|e| e.to_string())
}

#[tauri::command]
fn delete_ssh_host(state: State<AppState>, id: String) -> CmdResult<()> {
    state.stores.delete_ssh_host(&id).map_err(|e| e.to_string())
}

/// Parses `~/.ssh/config` into import candidates for the UI to review.
/// Nothing is saved here — the frontend lets the user pick, then saves each
/// via the normal `save_ssh_host`. A missing config file is just "no
/// candidates", not an error.
#[tauri::command]
fn read_ssh_config() -> CmdResult<Vec<SshHost>> {
    let Some(path) = dirs::home_dir().map(|h| h.join(".ssh").join("config")) else {
        return Ok(Vec::new());
    };
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.to_string()),
    };
    // ssh defaults a missing `User` to the local login name; mirror that.
    let default_user = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_default();
    Ok(senju_core::ssh_config::parse_ssh_config(&content, &default_user))
}

#[tauri::command]
fn get_session_snapshot(state: State<AppState>) -> SessionSnapshot {
    state.stores.session_snapshot()
}

#[tauri::command]
fn save_session_snapshot(state: State<AppState>, snapshot: SessionSnapshot) -> CmdResult<()> {
    state
        .stores
        .save_session_snapshot(&snapshot)
        .map_err(|e| e.to_string())
}

// -- Command history ----------------------------------------------------------------

#[tauri::command]
fn list_history(state: State<AppState>) -> Vec<HistoryEntry> {
    state.stores.history()
}

/// Records a finished command block. `meta` carries where it ran, its exit
/// status and duration — all optional, so the frontend can send what the
/// shell integration actually reported.
#[tauri::command]
fn add_history_entry(
    state: State<AppState>,
    command: String,
    kind: String,
    meta: Option<HistoryMeta>,
) -> CmdResult<()> {
    state
        .stores
        .add_history(&command, &kind, &meta.unwrap_or_default())
        .map_err(|e| e.to_string())
}

// -- Terminal profiles --------------------------------------------------------------

#[tauri::command]
fn list_profiles(state: State<AppState>) -> Vec<Profile> {
    state.stores.list_profiles()
}

#[tauri::command]
fn save_profile(state: State<AppState>, profile: Profile) -> CmdResult<Profile> {
    state.stores.save_profile(profile).map_err(|e| e.to_string())
}

#[tauri::command]
fn delete_profile(state: State<AppState>, id: String) -> CmdResult<()> {
    state.stores.delete_profile(&id).map_err(|e| e.to_string())
}

// -- Launch sets ----------------------------------------------------------------------

#[tauri::command]
fn list_launch_sets(state: State<AppState>) -> Vec<LaunchSet> {
    state.stores.list_launch_sets()
}

#[tauri::command]
fn save_launch_set(state: State<AppState>, set: LaunchSet) -> CmdResult<LaunchSet> {
    state
        .stores
        .save_launch_set(set)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn delete_launch_set(state: State<AppState>, id: String) -> CmdResult<()> {
    state
        .stores
        .delete_launch_set(&id)
        .map_err(|e| e.to_string())
}

// -- Settings -----------------------------------------------------------------------

/// Settings for the editor. The AI API key is blanked before it crosses into
/// the webview — the page has no use for the value, only for whether one is
/// saved, and a secret that never reaches the frontend cannot leak from it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SettingsView {
    #[serde(flatten)]
    settings: Settings,
    /// Whether an AI API key is stored. Drives the editor's "saved — enter a
    /// new one to replace it" placeholder.
    ai_api_key_saved: bool,
}

#[tauri::command]
fn get_settings(state: State<AppState>) -> SettingsView {
    let mut settings = state.stores.settings();
    let ai_api_key_saved = !settings.ai_api_key.is_empty();
    settings.ai_api_key.clear();
    SettingsView { settings, ai_api_key_saved }
}

/// Absolute path of `settings.json`, for "show in file manager". The file may
/// not exist yet (nothing has been saved); the caller saves first in that case.
#[tauri::command]
fn settings_file_path(state: State<AppState>) -> String {
    state.stores.settings_path().to_string_lossy().into_owned()
}

/// Saves settings. A blank `ai_api_key` leaves the stored key alone (the
/// editor never had it to send back); `clear_ai_api_key` is the explicit
/// delete.
#[tauri::command]
fn save_settings(
    state: State<AppState>,
    settings: Settings,
    clear_ai_api_key: Option<bool>,
) -> CmdResult<()> {
    if clear_ai_api_key.unwrap_or(false) {
        state.stores.clear_ai_api_key().map_err(|e| e.to_string())?;
    }
    state.stores.save_settings(&settings).map_err(|e| e.to_string())?;
    state.logger.configure(log_config(&state.default_log_dir, &settings));
    Ok(())
}

/* ---------------- settings export / import ---------------- */

/// Writes the whole configuration to `path` as one versioned JSON file.
///
/// Credentials are stripped by `share::to_config_json`, so the result can be
/// shared, committed or backed up — which is the point of keeping settings in
/// files the user can copy. The path comes from a native save dialog.
#[tauri::command]
fn export_settings(state: State<AppState>, path: String) -> CmdResult<()> {
    let json = senju_core::share::to_config_json(
        &state.stores.settings(),
        &state.stores.list_workflows(),
        &state.stores.list_ssh_hosts(),
        &state.stores.list_profiles(),
        &state.stores.list_launch_sets(),
        &state.stores.list_dev_servers(),
    );
    std::fs::write(&path, json).map_err(|e| e.to_string())
}

/// What an export file contains, for the confirmation the UI shows before
/// importing. Reading is separate from applying so the user sees what they are
/// about to merge rather than finding out afterwards.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ImportPreview {
    settings: bool,
    workflows: usize,
    ssh_hosts: usize,
    profiles: usize,
    launch_sets: usize,
    dev_servers: usize,
}

#[tauri::command]
fn read_settings_file(path: String) -> CmdResult<ImportPreview> {
    let content = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let export = senju_core::share::from_config_json(&content)?;
    Ok(ImportPreview {
        settings: export.settings.is_some(),
        workflows: export.workflows.len(),
        ssh_hosts: export.ssh_hosts.len(),
        profiles: export.profiles.len(),
        launch_sets: export.launch_sets.len(),
        dev_servers: export.dev_servers.len(),
    })
}

/// Merges an export file into the store.
///
/// Entries are upserted by id, so importing the same file twice does not
/// duplicate anything and a setup restored onto a machine keeps its own extra
/// hosts and workflows. Ids are preserved for the same reason the export keeps
/// them: a launch set points at a profile by id, a jump chain at hosts by id.
///
/// The saved AI API key is left alone — the file never carries one, and
/// clearing the key on this machine because an import did not bring one would
/// be a surprise.
#[tauri::command]
fn import_settings(state: State<AppState>, path: String) -> CmdResult<ImportPreview> {
    let content = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let export = senju_core::share::from_config_json(&content)?;
    let preview = ImportPreview {
        settings: export.settings.is_some(),
        workflows: export.workflows.len(),
        ssh_hosts: export.ssh_hosts.len(),
        profiles: export.profiles.len(),
        launch_sets: export.launch_sets.len(),
        dev_servers: export.dev_servers.len(),
    };
    if let Some(mut settings) = export.settings {
        // Blank means "leave the stored key alone" in `save_settings`, and the
        // export never carries one, so this is already the right shape.
        settings.ai_api_key = String::new();
        state.stores.save_settings(&settings).map_err(|e| e.to_string())?;
    }
    for workflow in export.workflows {
        state.stores.save_workflow(workflow).map_err(|e| e.to_string())?;
    }
    for host in export.ssh_hosts {
        state.stores.save_ssh_host(host).map_err(|e| e.to_string())?;
    }
    for profile in export.profiles {
        state.stores.save_profile(profile).map_err(|e| e.to_string())?;
    }
    for set in export.launch_sets {
        state.stores.save_launch_set(set).map_err(|e| e.to_string())?;
    }
    for preset in export.dev_servers {
        state.stores.save_dev_server(preset).map_err(|e| e.to_string())?;
    }
    Ok(preview)
}

// -- AI command assistance ----------------------------------------------------------

/// Asks the Anthropic API for a shell-command suggestion. The request carries
/// only the user's query plus OS/shell names — never terminal contents (see
/// `senju_core::ai`). Settings are read and the guard dropped before the
/// network await so the store mutex is not held across it.
#[tauri::command]
async fn ai_suggest_command(state: State<'_, AppState>, query: String) -> CmdResult<senju_core::ai::AiSuggestion> {
    let settings = state.stores.settings();
    let key = settings.ai_api_key.trim().to_string();
    if key.is_empty() {
        return Err("no-api-key".into());
    }
    let shell = if settings.shell.trim().is_empty() {
        if cfg!(windows) { "powershell".to_string() } else {
            std::env::var("SHELL")
                .ok()
                .and_then(|s| s.rsplit('/').next().map(str::to_string))
                .unwrap_or_else(|| "sh".into())
        }
    } else {
        settings.shell.trim().to_string()
    };
    let body = senju_core::ai::build_request(&query, std::env::consts::OS, &shell, &settings.ai_model);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(180))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client
        .post(senju_core::ai::ANTHROPIC_API_URL)
        .header("x-api-key", key)
        .header("anthropic-version", senju_core::ai::ANTHROPIC_VERSION)
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("network error: {e}"))?;
    let text = resp.text().await.map_err(|e| format!("network error: {e}"))?;
    senju_core::ai::parse_response(&text)
}

/// Saved window geometry. Persisted as `window-state.json` next to the other
/// stores so size/position/maximized survive restarts — a tiny replacement for
/// tauri-plugin-window-state, reusing the app's own JSON persistence.
#[derive(Serialize, Deserialize, Default)]
struct WindowState {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    maximized: bool,
}

fn window_state_path(app: &AppHandle) -> Option<std::path::PathBuf> {
    app.path()
        .app_config_dir()
        .ok()
        .map(|d| d.join("window-state.json"))
}

fn restore_window_state(window: &tauri::WebviewWindow) {
    let Some(path) = window_state_path(window.app_handle()) else {
        return;
    };
    let Ok(bytes) = std::fs::read(&path) else {
        return;
    };
    let Ok(st) = serde_json::from_slice::<WindowState>(&bytes) else {
        return;
    };
    if st.width > 0 && st.height > 0 {
        let _ = window.set_size(PhysicalSize::new(st.width, st.height));
        let _ = window.set_position(PhysicalPosition::new(st.x, st.y));
    }
    if st.maximized {
        let _ = window.maximize();
    }
}

fn save_window_state(window: &tauri::Window) {
    let Some(path) = window_state_path(window.app_handle()) else {
        return;
    };
    let maximized = window.is_maximized().unwrap_or(false);
    let (Ok(pos), Ok(size)) = (window.outer_position(), window.outer_size()) else {
        return;
    };
    let st = WindowState {
        x: pos.x,
        y: pos.y,
        width: size.width,
        height: size.height,
        maximized,
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_vec_pretty(&st) {
        let _ = std::fs::write(&path, json);
    }
}

/// Shows a native desktop notification. The webview decides *when* (a long
/// command finished in a hidden thread — see `maybeNotifyBlockDone` in
/// ui/app.js); this side only renders it through the OS notification center.
#[tauri::command]
fn notify(app: tauri::AppHandle, title: String, body: String) -> Result<(), String> {
    use tauri_plugin_notification::NotificationExt;
    app.notification()
        .builder()
        .title(title)
        .body(body)
        .show()
        .map_err(|e| e.to_string())
}

pub fn run() {
    // Which channel this copy came from decides whether the app may update
    // itself. `bundle_type()` is the marker the Tauri bundler stamps into each
    // installer's copy of the binary, so it distinguishes an NSIS build from an
    // MSI build compiled in the same run.
    let bundle = tauri::utils::platform::bundle_type().map(|b| b.to_string());
    let channel = distribution::current(bundle.as_deref());

    let mut builder = tauri::Builder::default()
        // Native save/open dialogs for workflow export/import.
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_notification::init());

    // Registered only where in-app updates are permitted. Leaving the plugin
    // out of a Store or package-manager build is stronger than hiding the
    // button: there is then no `updater` command for anything in the webview to
    // call, so the app physically cannot replace a managed install.
    if channel.in_app_updates() {
        builder = builder.plugin(tauri_plugin_updater::Builder::new().build());
    }

    builder
        .setup(move |app| {
            let dir = app.path().app_config_dir()?;
            let stores = Stores::new(dir.clone())?;
            let sink = TauriSink::new(app.handle().clone());
            let default_log_dir = dir.join("logs");
            let logger = Arc::new(SessionLogger::new(log_config(
                &default_log_dir,
                &stores.settings(),
            )));
            // The manager writes to the logging decorator, which copies to disk
            // and forwards to the webview sink. The bare sink is kept too: the
            // webview's binary channel is registered on it directly.
            let sessions =
                SessionManager::new(Arc::new(LoggingSink::new(sink.clone(), logger.clone())));
            app.manage(AppState {
                stores,
                sessions,
                channel,
                sink,
                logger,
                default_log_dir,
                sftp_cancels: Arc::new(Mutex::new(HashMap::new())),
                dev_servers: DevServerManager::new(Arc::new(DevServerEvents {
                    app: app.handle().clone(),
                })),
            });
            // Restore the saved window geometry before the first paint.
            if let Some(window) = app.get_webview_window("main") {
                restore_window_state(&window);
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() != "main" {
                return;
            }
            match event {
                // Persist geometry on close (our custom ✕ calls window.close(),
                // which fires this before the window goes away).
                tauri::WindowEvent::CloseRequested { .. } => save_window_state(window),
                // Only tear down all sessions when the MAIN window closes — not
                // when a future auxiliary window is destroyed.
                tauri::WindowEvent::Destroyed => {
                    if let Some(state) = window.app_handle().try_state::<AppState>() {
                        state.sessions.kill_all();
                        // A listener left bound after the window closes would
                        // hold its port with no UI left to stop it.
                        state.dev_servers.stop_all();
                    }
                }
                _ => {}
            }
        })
        .invoke_handler(tauri::generate_handler![
            subscribe_output,
            create_local_session,
            create_ssh_session,
            ssh_saved_credentials,
            forget_ssh_credentials,
            test_ssh_connection,
            open_external,
            reveal_path,
            open_external_terminal,
            session_log_dir,
            list_known_hosts,
            get_workflow_inputs,
            workflow_builtins,
            save_workflow_inputs,
            remove_known_hosts,
            generate_ssh_key,
            list_public_keys,
            install_public_key,
            scan_tasks,
            export_settings,
            read_settings_file,
            import_settings,
            start_dev_server,
            stop_dev_server,
            list_dev_servers,
            list_dev_server_presets,
            save_dev_server_preset,
            delete_dev_server_preset,
            sftp_list,
            sftp_upload,
            sftp_upload_into,
            sftp_download,
            sftp_cancel,
            session_write,
            session_resize,
            session_kill,
            list_workflows,
            save_workflow,
            delete_workflow,
            reorder_workflows,
            export_workflows,
            read_workflows_file,
            workflow_placeholders,
            fill_workflow,
            list_ssh_hosts,
            save_ssh_host,
            delete_ssh_host,
            read_ssh_config,
            get_session_snapshot,
            save_session_snapshot,
            list_history,
            add_history_entry,
            ai_suggest_command,
            list_profiles,
            save_profile,
            delete_profile,
            list_launch_sets,
            save_launch_set,
            delete_launch_set,
            get_settings,
            settings_file_path,
            save_settings,
            dist_info,
            open_update_manager,
            notify
        ])
        .build(tauri::generate_context!())
        .expect("error while running senju-term")
        .run(|app, event| {
            // macOS quits through the app menu's ⌘Q, which terminates the
            // process without ever routing through the window's close path —
            // so the geometry saved in `CloseRequested` would be skipped and
            // every ⌘Q would lose the window size. Persist here as well; the
            // write is idempotent, so a normal close doing both is harmless.
            if let tauri::RunEvent::ExitRequested { .. } = event {
                if let Some(webview) = app.get_webview_window("main") {
                    save_window_state(&webview.as_ref().window());
                }
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parent_of_an_absolute_path() {
        assert_eq!(parent_path("/home/user/work"), Some("/home/user".into()));
        // One level below the root: the parent is "/", not "".
        assert_eq!(parent_path("/home"), Some("/".into()));
        // The root has no parent — this is what stops the UI offering "up"
        // forever, and what a naive rfind would get wrong.
        assert_eq!(parent_path("/"), None);
        // A trailing slash must not produce the directory itself as its own
        // parent, which would make "up" a no-op that looks like a hang.
        assert_eq!(parent_path("/home/user/"), Some("/home".into()));
    }

    #[test]
    fn tilde_expands_only_as_a_path_root() {
        let home = dirs::home_dir().expect("test host has a home directory");
        let home = home.display().to_string();
        assert_eq!(expand_home("~"), home);
        assert_eq!(expand_home("~/work/notes.md"), format!("{home}/work/notes.md"));
        // `~other` is another user's home, which needs a passwd lookup we do
        // not do. Guessing would reveal the wrong directory, so it is left
        // alone and fails the absolute-path check instead.
        assert_eq!(expand_home("~deploy/work"), "~deploy/work");
        // A tilde anywhere but the front is an ordinary character (and a
        // legal one in a filename).
        assert_eq!(expand_home("/tmp/~backup"), "/tmp/~backup");
        assert_eq!(expand_home("/usr/local/bin"), "/usr/local/bin");
    }

    #[test]
    fn reveal_refuses_relative_and_missing_paths() {
        // Relative paths cannot be resolved without knowing the shell's cwd,
        // which we do not track — revealing the app's own cwd instead would
        // be worse than refusing.
        assert!(reveal_path("src/lib.rs".into()).is_err());
        assert!(reveal_path("".into()).is_err());
        // Absolute but absent: refused rather than revealing whatever
        // ancestor the file manager settles on.
        let missing = std::env::temp_dir().join("senju-does-not-exist-9d2f1a");
        assert!(reveal_path(missing.display().to_string()).is_err());
    }

    #[test]
    fn frames_carry_id_and_payload() {
        let id = "6f1b2c3d-4e5f-4a6b-8c9d-0e1f2a3b4c5d";
        let f = frame(FRAME_DATA, id, b"hello\r\n").unwrap();
        assert_eq!(f[0], FRAME_DATA);
        assert_eq!(f[1] as usize, id.len());
        assert_eq!(&f[2..2 + id.len()], id.as_bytes());
        assert_eq!(&f[2 + id.len()..], b"hello\r\n");
    }

    #[test]
    fn exit_code_round_trips_as_little_endian() {
        let f = frame(FRAME_EXIT, "s", &(-1i32).to_le_bytes()).unwrap();
        let payload = &f[2 + 1..];
        assert_eq!(i32::from_le_bytes(payload.try_into().unwrap()), -1);
    }

    #[test]
    fn payload_bytes_are_not_reinterpreted() {
        // A frame must survive bytes that are not valid UTF-8 — the reason the
        // old transport base64-encoded everything.
        let raw = [0x1b, 0x5b, 0x33, 0x31, 0x6d, 0xe3, 0x81, 0xff, 0x00];
        let f = frame(FRAME_DATA, "abc", &raw).unwrap();
        assert_eq!(&f[2 + 3..], &raw);
    }

    #[test]
    fn over_long_ids_are_refused_rather_than_truncated() {
        let id = "x".repeat(256);
        assert!(frame(FRAME_DATA, &id, b"x").is_none());
    }
}
