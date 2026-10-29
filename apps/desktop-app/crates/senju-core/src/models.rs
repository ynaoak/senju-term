use serde::{Deserialize, Serialize};

/// A saved custom command ("workflow" in Warp terms). The command string may
/// contain `{{name}}` or `{{name:default}}` placeholders that are filled in
/// at run time.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Workflow {
    #[serde(default)]
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub command: String,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Group path this workflow belongs to, used to organize the panel and to
    /// build the right-click launcher's hierarchical (flyout) menu. Nesting is
    /// expressed with `/` — e.g. `"Git"` or `"Deploy/Staging"`. Empty means
    /// ungrouped (shown at the top level). The order of workflows within a
    /// group follows their order in the stored list (see `reorder_workflows`).
    #[serde(default)]
    pub group: String,
    /// Optional keyboard shortcut that runs this workflow, stored normalized
    /// as e.g. `"ctrl+shift+g"`. Empty means no shortcut.
    #[serde(default)]
    pub shortcut: String,
    /// When true, the workflow is shown as a quick-launch button in the shell
    /// view's workflow bar.
    #[serde(default)]
    pub show_button: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum SshAuthMethod {
    #[default]
    Password,
    Key,
    Agent,
    /// Multi-step auth: public key first, then password — for servers
    /// configured with `AuthenticationMethods publickey,password`.
    KeyPassword,
}

/// A saved SSH destination. Secrets (password / key passphrase) are never
/// persisted; they are collected in the UI at connect time.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SshHost {
    #[serde(default)]
    pub id: String,
    pub name: String,
    pub host: String,
    #[serde(default = "default_ssh_port")]
    pub port: u16,
    pub username: String,
    #[serde(default)]
    pub auth_method: SshAuthMethod,
    /// Path to the private key when `auth_method == Key`. `~` is expanded.
    #[serde(default)]
    pub key_path: String,
    /// Port forwards started automatically when a session to this host
    /// connects and stopped when it ends — one line each, in `ssh` shape:
    ///
    /// ```text
    ///   8080:localhost:80        local   (-L; the bare form, kept for
    ///   -L 8080:localhost:80              hosts saved before -R/-D existed)
    ///   -R 9000:localhost:3000   remote  (-R)
    ///   -D 1080                  dynamic (-D, SOCKS5)
    /// ```
    ///
    /// Every listener binds loopback — locally for `-L`/`-D`, on the server
    /// for `-R`. Parsed by `sessions::parse_forward_spec`.
    #[serde(default)]
    pub forwards: Vec<String>,
    /// Remember this host's password / passphrase so connecting does not ask
    /// for it again. **Opt-in and off by default**: the app does not persist
    /// credentials unless asked, and this is the user overriding that for one
    /// host, not a default anyone gets by accident.
    ///
    /// The values sit in `password` / `passphrase` below, in this same
    /// `ssh-hosts.json`, in the clear. Settings export strips them
    /// (`share::to_config_json`), so a shared or committed export never
    /// carries them — but anyone who can read the app's config directory can
    /// read them, which is the trade this option asks the user to accept.
    #[serde(default)]
    pub save_credentials: bool,
    /// Password for `SshAuthMethod::Password`, kept only while
    /// `save_credentials` is on and erased the moment it is turned off.
    #[serde(default)]
    pub password: String,
    /// Passphrase for an encrypted private key, on the same terms.
    #[serde(default)]
    pub passphrase: String,
    /// Ordered ids of other saved SSH hosts to connect *through* before
    /// reaching this one — the multi-hop / ProxyJump (`ssh -J`) chain. The
    /// first id is the first hop from the client; the last hop opens the
    /// tunnel that this host is reached over. Empty = direct connection.
    /// Resolved and validated by `sessions::resolve_jump_chain`.
    #[serde(default)]
    pub jump_hosts: Vec<String>,
    /// Group path for the hosts panel, `/`-nested exactly like
    /// [`Workflow::group`] (`"Prod/Web"`). Empty = ungrouped. The panel groups
    /// hosts under it and offers connect-all / run-on-all per group, so it is
    /// the unit of "these machines go together". Normalized on save.
    #[serde(default)]
    pub group: String,
    /// Free-form labels, searched alongside name / host / user.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Label color: one of [`HOST_COLORS`], or empty for none. Shown on the
    /// host card, on its thread rows, and as a frame around the pane while a
    /// session to it is on screen — so a production box is visibly different
    /// from a staging one before anything is typed. Named rather than hex so
    /// each theme can pick a readable shade and a typo cannot paint a pane an
    /// invisible color; `store::save_ssh_host` rejects anything else.
    #[serde(default)]
    pub color: String,
    /// Write every session to this host into a log file (see
    /// `session_log`). Off by default: a transcript of a root shell is not
    /// something to leave on disk without being asked. Per host rather than
    /// global because the hosts that need an audit trail are rarely all of
    /// them.
    #[serde(default)]
    pub log_sessions: bool,
    /// Reconnect by itself when the connection drops mid-session (not on a
    /// clean `exit`, and never at app start — session restore still skips
    /// SSH). Off by default and per host: an unattended reconnect retries
    /// authentication against a server, which is not something every host
    /// should get without being asked. Saved credentials are reused when
    /// present; otherwise the first retry prompts once.
    #[serde(default)]
    pub auto_reconnect: bool,
}

/// The label colors a host may carry (`SshHost::color`). The UI maps each to
/// a per-theme token; the store rejects any other value.
pub const HOST_COLORS: &[&str] = &["red", "orange", "yellow", "green", "blue", "purple"];

/// A saved built-in dev server: the directory, port and options to start it
/// with. Only the *settings* are stored — whether a server is running lives in
/// `devserver::DevServerManager` and dies with the process, so a preset can
/// never claim to be serving something it is not.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct DevServerPreset {
    #[serde(default)]
    pub id: String,
    pub name: String,
    /// Directory to serve. `~` expands, like a profile's `cwd`.
    pub root: String,
    /// 0 means "ask the OS for a free port" — the same meaning it has in
    /// `DevServerSpec`, so a preset can avoid fighting over a number.
    #[serde(default)]
    pub port: u16,
    #[serde(default)]
    pub spa: bool,
    #[serde(default = "default_hot_reload")]
    pub hot_reload: bool,
    /// Open the browser once it is listening.
    #[serde(default)]
    pub open_browser: bool,
}

fn default_hot_reload() -> bool {
    true
}

fn default_ssh_port() -> u16 {
    22
}

/// A named local-shell profile, à la Windows Terminal. Users pick which
/// profile a new local thread launches, and one profile is the default.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Profile {
    #[serde(default)]
    pub id: String,
    pub name: String,
    /// Executable to launch. Empty means the OS default shell
    /// (`$SHELL` on Unix; on Windows `pwsh.exe` if installed, else
    /// `powershell.exe` — never cmd.exe, which can't take shell integration).
    #[serde(default)]
    pub command: String,
    /// Arguments passed to the executable.
    #[serde(default)]
    pub args: Vec<String>,
    /// Working directory. Empty means the user's home directory. `~` expands.
    #[serde(default)]
    pub cwd: String,
}

/// One shell/connection to open as part of a [`LaunchSet`]. Exactly one of
/// `profile_id` / `ssh_host_id` is meant to be set (a local profile or an SSH
/// host); the UI enforces that exclusivity, the model itself doesn't. An
/// empty `profile_id` with an empty `ssh_host_id` falls back to the OS
/// default shell, same as an unset profile elsewhere in the app.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct LaunchSetItem {
    #[serde(default)]
    pub profile_id: String,
    #[serde(default)]
    pub ssh_host_id: String,
    /// Workflow run immediately after the shell/connection is ready. Empty
    /// means just open the shell with nothing auto-run.
    #[serde(default)]
    pub workflow_id: String,
    /// Ad-hoc command run after the shell/connection is ready, without having
    /// to save it as a workflow first. It runs *after* `workflow_id` when both
    /// are set, so a set can pair a saved workflow with a one-off follow-up.
    /// Supports the same `{{name}}` / `{{name:default}}` placeholders as a
    /// workflow. Empty means no direct command.
    #[serde(default)]
    pub command: String,
}

/// A named, ordered set of shells/connections (each optionally paired with a
/// workflow and/or a direct command to auto-run) that the user launches
/// together in one action — e.g. "毎朝の環境" opening a local shell, an SSH
/// host, and a log-tail workflow at once.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LaunchSet {
    #[serde(default)]
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub items: Vec<LaunchSetItem>,
}

/// One thread in the saved session layout: which profile (local) or SSH
/// host it was running. Exactly one side is set, mirroring LaunchSetItem.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct SnapshotThread {
    #[serde(default)]
    pub profile_id: String,
    #[serde(default)]
    pub ssh_host_id: String,
}

/// The thread layout at last shutdown, used to restore the workspace on the
/// next launch (when Settings::restore_session is on). Local threads are
/// recreated from their profile; SSH threads are never auto-reconnected
/// (they need credentials), the UI only reports how many were skipped.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct SessionSnapshot {
    #[serde(default)]
    pub threads: Vec<SnapshotThread>,
}

/// One executed command, captured from a finished OSC 133 command block
/// (local or SSH — anything that emits the markers). `at` is unix seconds.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HistoryEntry {
    pub command: String,
    /// "local" or "ssh" — where it ran, for display only.
    #[serde(default)]
    pub kind: String,
    /// Unix seconds of the most recent run.
    #[serde(default)]
    pub at: u64,
    /// Where it ran: the saved SSH host's name for SSH threads, the profile
    /// name for local ones (empty when unknown). Filterable in the palette.
    #[serde(default)]
    pub host: String,
    /// Exit status of the most recent run, when the shell reported one via
    /// OSC 133 `D;<code>` (`None` for older entries or a bare `D`).
    #[serde(default)]
    pub exit: Option<i32>,
    /// Wall-clock duration of the most recent run in milliseconds (0 when
    /// unknown).
    #[serde(default)]
    pub duration_ms: u64,
    /// How many times this command has been recorded. Re-running an
    /// existing command bumps this instead of adding a second entry.
    #[serde(default = "default_runs")]
    pub runs: u32,
}

fn default_runs() -> u32 {
    1
}

/// Per-run details recorded alongside a history command (see
/// [`crate::Stores::add_history`]).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct HistoryMeta {
    #[serde(default)]
    pub host: String,
    #[serde(default)]
    pub exit: Option<i32>,
    #[serde(default)]
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Settings {
    #[serde(default = "default_font_size")]
    pub font_size: u16,
    /// Legacy single-shell override; kept for backward compatibility and used
    /// as a fallback when no profiles exist. Empty means OS default.
    #[serde(default)]
    pub shell: String,
    /// Id of the profile launched for new local threads when none is chosen.
    #[serde(default)]
    pub default_profile_id: String,
    /// Terminal font family override. Empty means the built-in default stack.
    #[serde(default)]
    pub font_family: String,
    /// Terminal scrollback size, in lines.
    #[serde(default = "default_scrollback")]
    pub scrollback: u32,
    /// UI color theme: "dark" (default) or "light". Stored as a free string
    /// so older settings files (missing the field) deserialize to the default
    /// and future themes don't need a schema change.
    #[serde(default = "default_theme")]
    pub theme: String,
    /// UI language: "ja" (default) or "en". A free string for the same reason
    /// as `theme` — older settings files default to Japanese, and adding a
    /// language later needs no schema change.
    #[serde(default = "default_language")]
    pub language: String,
    /// Terminal color scheme, independent of the UI theme: "" follows the
    /// UI theme (dark/light), a preset id ("solarized-dark", "dracula", …)
    /// picks a built-in palette, and "custom" uses `term_colors`. Free
    /// string so presets can be added without a schema change; unknown ids
    /// fall back to following the UI theme.
    #[serde(default)]
    pub term_theme: String,
    /// User-edited palette for `term_theme == "custom"`: xterm theme keys
    /// (`background`, `foreground`, `cursor`, `black` … `brightWhite`) to
    /// `#rrggbb`. Keys left out fall back to the UI theme's palette.
    #[serde(default)]
    pub term_colors: std::collections::BTreeMap<String, String>,
    /// Recreate the previous session's local threads on startup from the
    /// saved snapshot. Defaults on; SSH threads are reported, not
    /// reconnected (credentials are never persisted).
    #[serde(default = "default_restore_session")]
    pub restore_session: bool,
    /// GPU-accelerated terminal rendering via the xterm WebGL addon.
    /// Defaults on; a checkbox in settings turns it off for machines where
    /// WebGL misbehaves (old GPUs, driver blocklists, remote desktops).
    #[serde(default = "default_gpu_rendering")]
    pub gpu_rendering: bool,
    /// Auto-inject OSC 133 shell-integration hooks (command-block markers)
    /// into recognized local shells (bash/zsh/fish) at launch, without
    /// touching the user's own rc files. Defaults on; older settings files
    /// (missing the field) also default on.
    #[serde(default = "default_shell_integration")]
    pub shell_integration: bool,
    /// Anthropic API key for AI command assistance. Empty (default) disables
    /// the feature.
    ///
    /// Persisted here, in settings.json, in the clear. Settings export strips
    /// it (`share::to_config_json`) so a shared export never carries it, but
    /// anyone who can read the app's config directory can read the key.
    ///
    /// Never sent anywhere except directly to the Anthropic API, and never
    /// handed to the webview — the UI is told only whether one is saved.
    #[serde(default)]
    pub ai_api_key: String,
    /// Model used for AI command assistance. Empty falls back to the
    /// built-in default (see `ai::DEFAULT_AI_MODEL`).
    #[serde(default)]
    pub ai_model: String,
    /// Check GitHub Releases for a signed update shortly after startup.
    /// Defaults on; the check is silent unless an update is found, and a
    /// build without updater signing configured simply no-ops.
    #[serde(default = "default_auto_update_check")]
    pub auto_update_check: bool,
    /// Desktop notification when a command block finishes after running for
    /// at least `notify_threshold_secs`, but only when the user is not
    /// looking at it (the thread is hidden or the window is unfocused).
    /// Needs OSC 133 shell integration to know when a command ends.
    #[serde(default = "default_notify_long_commands")]
    pub notify_long_commands: bool,
    /// Minimum command duration, in seconds, before a completion notification
    /// is sent. Zero notifies for every finished command.
    #[serde(default = "default_notify_threshold_secs")]
    pub notify_threshold_secs: u32,
    /// Where session logs go. Empty = `<config dir>/logs`. `~` expands.
    #[serde(default)]
    pub session_log_dir: String,
    /// Strip escape sequences so the log is readable text (default). Off
    /// keeps the raw byte stream, colors and all, for replaying in a
    /// terminal.
    #[serde(default = "default_session_log_plain")]
    pub session_log_plain: bool,
    /// Also log local shells, not only the SSH hosts that opted in. Off by
    /// default for the same reason `SshHost::log_sessions` is.
    #[serde(default)]
    pub session_log_local: bool,
    /// What a right-click in a terminal pane does: "copy_paste" copies the
    /// selection (or pastes when there is none), as Windows Terminal does;
    /// "menu" opens the workflow launcher menu. Empty (or any unknown value)
    /// follows the platform default — copy/paste on Windows and Linux, the
    /// menu on macOS. Shift+right-click opens the menu either way. Free
    /// string so more actions can be added without a schema change.
    #[serde(default)]
    pub right_click: String,
    /// Command line used by "open in external terminal". Empty means the OS
    /// default terminal. `{dir}` in it is replaced by the target directory;
    /// the process is always started with that directory as its working
    /// directory too. No shell is involved: it is split into program and
    /// arguments, with double quotes grouping words that contain spaces.
    #[serde(default)]
    pub external_terminal: String,
    /// Command-palette key bindings, edited by hand in settings.json: action id
    /// ("open", "next", "prev", "run", "insert", "save", "close") → key combos
    /// such as "ctrl+shift+p". An action left out (or given an empty list) keeps
    /// its built-in keys; unknown actions and unparsable combos are ignored by the
    /// UI (which warns). Free-form here so the UI owns the key syntax.
    #[serde(default)]
    pub palette_keys: std::collections::BTreeMap<String, Vec<String>>,
}

fn default_session_log_plain() -> bool {
    true
}

fn default_font_size() -> u16 {
    14
}

fn default_scrollback() -> u32 {
    10000
}

fn default_theme() -> String {
    "dark".into()
}

fn default_language() -> String {
    "ja".into()
}

fn default_shell_integration() -> bool {
    true
}

fn default_gpu_rendering() -> bool {
    true
}

fn default_restore_session() -> bool {
    true
}

fn default_auto_update_check() -> bool {
    true
}

fn default_notify_long_commands() -> bool {
    true
}

fn default_notify_threshold_secs() -> u32 {
    10
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            font_size: default_font_size(),
            shell: String::new(),
            default_profile_id: String::new(),
            font_family: String::new(),
            scrollback: default_scrollback(),
            theme: default_theme(),
            language: default_language(),
            term_theme: String::new(),
            term_colors: Default::default(),
            restore_session: default_restore_session(),
            gpu_rendering: default_gpu_rendering(),
            shell_integration: default_shell_integration(),
            ai_api_key: String::new(),
            ai_model: String::new(),
            auto_update_check: default_auto_update_check(),
            notify_long_commands: default_notify_long_commands(),
            notify_threshold_secs: default_notify_threshold_secs(),
            session_log_dir: String::new(),
            session_log_plain: default_session_log_plain(),
            session_log_local: false,
            right_click: String::new(),
            external_terminal: String::new(),
            palette_keys: Default::default(),
        }
    }
}
