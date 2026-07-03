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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Settings {
    #[serde(default = "default_font_size")]
    pub font_size: u16,
    /// Override for the local shell; empty means use $SHELL / OS default.
    #[serde(default)]
    pub shell: String,
}

fn default_font_size() -> u16 {
    14
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            font_size: default_font_size(),
            shell: String::new(),
        }
    }
}
