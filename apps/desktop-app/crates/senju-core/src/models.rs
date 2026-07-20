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
}

/// A named, ordered set of shells/connections (each optionally paired with a
/// workflow to auto-run) that the user launches together in one action —
/// e.g. "毎朝の環境" opening a local shell, an SSH host, and a log-tail
/// workflow at once.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LaunchSet {
    #[serde(default)]
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub items: Vec<LaunchSetItem>,
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
    /// Auto-inject OSC 133 shell-integration hooks (command-block markers)
    /// into recognized local shells (bash/zsh/fish) at launch, without
    /// touching the user's own rc files. Defaults on; older settings files
    /// (missing the field) also default on.
    #[serde(default = "default_shell_integration")]
    pub shell_integration: bool,
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

fn default_shell_integration() -> bool {
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
            shell_integration: default_shell_integration(),
        }
    }
}
