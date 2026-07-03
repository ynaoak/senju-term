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
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum SshAuthMethod {
    #[default]
    Password,
    Key,
    Agent,
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
}

fn default_font_size() -> u16 {
    14
}

fn default_scrollback() -> u32 {
    10000
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            font_size: default_font_size(),
            shell: String::new(),
            default_profile_id: String::new(),
            font_family: String::new(),
            scrollback: default_scrollback(),
        }
    }
}
