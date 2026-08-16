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
    /// Local port forwards (`ssh -L` style, one
    /// `local_port:remote_host:remote_port` string each) started
    /// automatically when a session to this host connects and stopped when it
    /// ends. Listeners bind to 127.0.0.1 only. Parsed by
    /// `sessions::parse_forward_spec`.
    #[serde(default)]
    pub forwards: Vec<String>,
    /// Ordered ids of other saved SSH hosts to connect *through* before
    /// reaching this one — the multi-hop / ProxyJump (`ssh -J`) chain. The
    /// first id is the first hop from the client; the last hop opens the
    /// tunnel that this host is reached over. Empty = direct connection.
    /// Resolved and validated by `sessions::resolve_jump_chain`.
    #[serde(default)]
    pub jump_hosts: Vec<String>,
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
    /// (`$SHELL` on Unix, `%COMSPEC%`/PowerShell on Windows).
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
    #[serde(default)]
    pub at: u64,
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
    /// API key for AI command assistance (the user's own Anthropic key).
    /// Empty (default) disables the feature. Stored in the local settings
    /// file — never sent anywhere except directly to the Anthropic API.
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
            restore_session: default_restore_session(),
            gpu_rendering: default_gpu_rendering(),
            shell_integration: default_shell_integration(),
            ai_api_key: String::new(),
            ai_model: String::new(),
            auto_update_check: default_auto_update_check(),
        }
    }
}
