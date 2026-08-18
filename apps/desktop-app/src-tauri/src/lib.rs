use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use serde::{Deserialize, Serialize};
use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, State};

use senju_core::distribution::{self, DistChannel};
use senju_core::sessions::{resolve_jump_chain, SessionInfo, SshSecrets, SshTestReport};
use senju_core::template;
use senju_core::models::{HistoryEntry, SessionSnapshot};
use senju_core::{LaunchSet, LocalSpec, Profile, SessionManager, Settings, SshHost, Stores, Workflow};

struct AppState {
    stores: Stores,
    sessions: SessionManager,
    /// Detected once at startup — see `senju_core::distribution`.
    channel: DistChannel,
    /// Kept alongside the manager so `subscribe_output` can hand the sink the
    /// webview's binary channel.
    sink: Arc<TauriSink>,
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
) -> CmdResult<SessionInfo> {
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
    let shell_integration = state.stores.settings().shell_integration;
    state
        .sessions
        .create_local(&spec, cols, rows, shell_integration)
        .map_err(|e| e.to_string())
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
) -> CmdResult<SessionInfo> {
    let all_hosts = state.stores.list_ssh_hosts();
    let target = all_hosts
        .iter()
        .find(|h| h.id == host_id)
        .ok_or_else(|| format!("unknown host: {host_id}"))?
        .clone();
    // Resolve (and validate) the jump chain here — the store is the source of
    // truth for what each jump-host id points at.
    let chain = resolve_jump_chain(&target, &all_hosts)?;
    let secrets_by_id: HashMap<String, SshSecrets> = secrets
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
    state
        .sessions
        .create_ssh_chain(&chain, &secrets_by_id, cols, rows, &fingerprints)
        .await
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

#[tauri::command]
fn fill_workflow(command: String, values: HashMap<String, String>) -> String {
    template::fill_placeholders(&command, &values)
}

// -- SSH hosts --------------------------------------------------------------------

#[tauri::command]
fn list_ssh_hosts(state: State<AppState>) -> Vec<SshHost> {
    state.stores.list_ssh_hosts()
}

#[tauri::command]
fn save_ssh_host(state: State<AppState>, host: SshHost) -> CmdResult<SshHost> {
    state.stores.save_ssh_host(host).map_err(|e| e.to_string())
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

#[tauri::command]
fn add_history_entry(state: State<AppState>, command: String, kind: String) -> CmdResult<()> {
    state
        .stores
        .add_history(&command, &kind)
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

#[tauri::command]
fn get_settings(state: State<AppState>) -> Settings {
    state.stores.settings()
}

#[tauri::command]
fn save_settings(state: State<AppState>, settings: Settings) -> CmdResult<()> {
    state
        .stores
        .save_settings(&settings)
        .map_err(|e| e.to_string())
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
        .plugin(tauri_plugin_process::init());

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
            let stores = Stores::new(dir)?;
            let sink = TauriSink::new(app.handle().clone());
            app.manage(AppState {
                stores,
                sessions: SessionManager::new(sink.clone()),
                channel,
                sink,
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
                    }
                }
                _ => {}
            }
        })
        .invoke_handler(tauri::generate_handler![
            subscribe_output,
            create_local_session,
            create_ssh_session,
            test_ssh_connection,
            open_external,
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
            save_settings,
            dist_info,
            open_update_manager
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
