use std::collections::HashMap;
use std::sync::Arc;

use base64::Engine;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

use senju_core::sessions::{SessionInfo, SshSecrets};
use senju_core::template;
use senju_core::{LocalSpec, Profile, SessionManager, Settings, SshHost, Stores, Workflow};

struct AppState {
    stores: Stores,
    sessions: SessionManager,
}

/// Forwards session output/exit to the webview. Output bytes are base64
/// encoded so multibyte sequences split across reads survive the JSON hop.
struct TauriSink {
    app: AppHandle,
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

impl senju_core::EventSink for TauriSink {
    fn data(&self, id: &str, data: &[u8]) {
        let encoded = base64::engine::general_purpose::STANDARD.encode(data);
        let _ = self.app.emit("session:data", DataEvent { id, data: encoded });
    }

    fn exit(&self, id: &str, code: i32) {
        let _ = self.app.emit("session:exit", ExitEvent { id, code });
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
    state
        .sessions
        .create_local(&spec, cols, rows)
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
    trust_host: Option<bool>,
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
            trust_host.unwrap_or(false),
        )
        .await
        .map_err(|e| e.to_string())
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

pub fn run() {
    tauri::Builder::default()
        // Remembers window size/position/maximized state across restarts,
        // restoring it before the window is first shown.
        .plugin(tauri_plugin_window_state::Builder::default().build())
        .setup(|app| {
            let dir = app.path().app_config_dir()?;
            let stores = Stores::new(dir)?;
            let sink = Arc::new(TauriSink {
                app: app.handle().clone(),
            });
            app.manage(AppState {
                stores,
                sessions: SessionManager::new(sink),
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::Destroyed = event {
                if let Some(state) = window.app_handle().try_state::<AppState>() {
                    state.sessions.kill_all();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            create_local_session,
            create_ssh_session,
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
            get_settings,
            save_settings
        ])
        .run(tauri::generate_context!())
        .expect("error while running senju-term");
}
