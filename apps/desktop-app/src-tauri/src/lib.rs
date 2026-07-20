use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, State};

use senju_core::sessions::{SessionInfo, SshSecrets, SshTestReport};
use senju_core::template;
use senju_core::{LaunchSet, LocalSpec, Profile, SessionManager, Settings, SshHost, Stores, Workflow};

struct AppState {
    stores: Stores,
    sessions: SessionManager,
}

/// Flush terminal output at most once every ~8ms per session (roughly a frame)
/// instead of once per PTY read, and force a flush past this size so a firehose
/// (`cat` of a big file, a build's output) can't grow an unbounded buffer.
const FLUSH_INTERVAL: Duration = Duration::from_millis(8);
const FLUSH_THRESHOLD: usize = 64 * 1024;

/// Forwards session output/exit to the webview. Output bytes are base64
/// encoded so multibyte sequences split across reads survive the JSON hop.
///
/// Reads are coalesced: instead of a base64+JSON+emit per PTY read (thousands
/// per second during heavy output, each with full event-system overhead and a
/// main-thread decode), bytes accumulate per session and a background flusher
/// emits one batched event per session per tick.
struct TauriSink {
    app: AppHandle,
    pending: Mutex<HashMap<String, Vec<u8>>>,
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
        });
        let flusher = sink.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(FLUSH_INTERVAL);
            flusher.flush_all();
        });
        sink
    }

    fn emit_data(&self, id: &str, bytes: &[u8]) {
        if bytes.is_empty() {
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
        let _ = self
            .app
            .emit_to("main", "session:exit", ExitEvent { id, code });
    }
}

type CmdResult<T> = Result<T, String>;

// -- Sessions -----------------------------------------------------------------

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

#[tauri::command]
async fn create_ssh_session(
    state: State<'_, AppState>,
    host_id: String,
    password: Option<String>,
    passphrase: Option<String>,
    cols: u16,
    rows: u16,
    // `Some(fp)` means: the host was unknown, the UI already showed the user
    // fingerprint `fp` (from a previous `UNKNOWN_HOST_KEY` failure) and the
    // user approved it. The backend only trusts and records the key if the
    // one presented on THIS handshake has that exact SHA256 fingerprint —
    // see `SessionManager::create_ssh` / `sessions::ssh` for why a plain
    // `trust_host: bool` was unsafe (TOFU TOCTOU).
    expected_fingerprint: Option<String>,
) -> CmdResult<SessionInfo> {
    let host = state
        .stores
        .list_ssh_hosts()
        .into_iter()
        .find(|h| h.id == host_id)
        .ok_or_else(|| format!("unknown host: {host_id}"))?;
    state
        .sessions
        .create_ssh(
            &host,
            SshSecrets { password, passphrase },
            cols,
            rows,
            expected_fingerprint,
        )
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
    tauri::Builder::default()
        .setup(|app| {
            let dir = app.path().app_config_dir()?;
            let stores = Stores::new(dir)?;
            let sink = TauriSink::new(app.handle().clone());
            app.manage(AppState {
                stores,
                sessions: SessionManager::new(sink),
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
            workflow_placeholders,
            fill_workflow,
            list_ssh_hosts,
            save_ssh_host,
            delete_ssh_host,
            list_profiles,
            save_profile,
            delete_profile,
            list_launch_sets,
            save_launch_set,
            delete_launch_set,
            get_settings,
            save_settings
        ])
        .run(tauri::generate_context!())
        .expect("error while running senju-term");
}
