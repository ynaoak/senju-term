//! JSON-file-backed persistence. Each collection lives in its own file under
//! the app config directory so users can inspect and back it up by hand.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{de::DeserializeOwned, Serialize};

use crate::models::{Profile, Settings, SshHost, Workflow};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serialization error: {0}")]
    Json(#[from] serde_json::Error),
}

pub struct Stores {
    dir: PathBuf,
    lock: Mutex<()>,
}

impl Stores {
    pub fn new(dir: impl Into<PathBuf>) -> Result<Self, StoreError> {
        let dir = dir.into();
        fs::create_dir_all(&dir)?;
        Ok(Self {
            dir,
            lock: Mutex::new(()),
        })
    }

    // -- Workflows -----------------------------------------------------------

    pub fn list_workflows(&self) -> Vec<Workflow> {
        self.read("workflows", default_workflows)
    }

    pub fn save_workflow(&self, mut wf: Workflow) -> Result<Workflow, StoreError> {
        if wf.id.is_empty() {
            wf.id = uuid::Uuid::new_v4().to_string();
        }
        let _g = self.lock.lock().unwrap();
        let mut items = self.read("workflows", default_workflows);
        upsert(&mut items, wf.clone(), |w| &w.id);
        self.write("workflows", &items)?;
        Ok(wf)
    }

    pub fn delete_workflow(&self, id: &str) -> Result<(), StoreError> {
        let _g = self.lock.lock().unwrap();
        let mut items = self.read("workflows", default_workflows);
        items.retain(|w| w.id != id);
        self.write("workflows", &items)
    }

    // -- SSH hosts ------------------------------------------------------------

    pub fn list_ssh_hosts(&self) -> Vec<SshHost> {
        self.read("ssh-hosts", Vec::new)
    }

    pub fn save_ssh_host(&self, mut host: SshHost) -> Result<SshHost, StoreError> {
        if host.id.is_empty() {
            host.id = uuid::Uuid::new_v4().to_string();
        }
        let _g = self.lock.lock().unwrap();
        let mut items = self.read("ssh-hosts", Vec::new);
        upsert(&mut items, host.clone(), |h| &h.id);
        self.write("ssh-hosts", &items)?;
        Ok(host)
    }

    pub fn delete_ssh_host(&self, id: &str) -> Result<(), StoreError> {
        let _g = self.lock.lock().unwrap();
        let mut items: Vec<SshHost> = self.read("ssh-hosts", Vec::new);
        items.retain(|h| h.id != id);
        self.write("ssh-hosts", &items)
    }

    // -- Terminal profiles ----------------------------------------------------

    pub fn list_profiles(&self) -> Vec<Profile> {
        self.read("profiles", default_profiles)
    }

    pub fn save_profile(&self, mut profile: Profile) -> Result<Profile, StoreError> {
        if profile.id.is_empty() {
            profile.id = uuid::Uuid::new_v4().to_string();
        }
        let _g = self.lock.lock().unwrap();
        let mut items = self.read("profiles", default_profiles);
        upsert(&mut items, profile.clone(), |p| &p.id);
        self.write("profiles", &items)?;
        Ok(profile)
    }

    pub fn delete_profile(&self, id: &str) -> Result<(), StoreError> {
        let _g = self.lock.lock().unwrap();
        let mut items = self.read("profiles", default_profiles);
        items.retain(|p| p.id != id);
        self.write("profiles", &items)
    }

    /// Resolves the profile to launch, trying in order: the requested id, the
    /// configured default, then the first profile. `None` only if no profiles
    /// exist. An unknown requested id falls through to the default.
    pub fn resolve_profile(&self, requested_id: Option<&str>) -> Option<Profile> {
        let profiles = self.list_profiles();
        let default_id = self.settings().default_profile_id;
        let pick = |id: &str| profiles.iter().find(|p| p.id == id).cloned();
        requested_id
            .filter(|s| !s.is_empty())
            .and_then(pick)
            .or_else(|| (!default_id.is_empty()).then(|| pick(&default_id)).flatten())
            .or_else(|| profiles.first().cloned())
    }

    // -- Settings -------------------------------------------------------------

    pub fn settings(&self) -> Settings {
        self.read("settings", Settings::default)
    }

    pub fn save_settings(&self, settings: &Settings) -> Result<(), StoreError> {
        let _g = self.lock.lock().unwrap();
        self.write("settings", settings)
    }

    // -- Internals -------------------------------------------------------------

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{name}.json"))
    }

    fn read<T: DeserializeOwned>(&self, name: &str, default: impl FnOnce() -> T) -> T {
        read_json(&self.path(name)).unwrap_or_else(default)
    }

    fn write<T: Serialize>(&self, name: &str, value: &T) -> Result<(), StoreError> {
        let path = self.path(name);
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_vec_pretty(value)?)?;
        fs::rename(&tmp, &path)?;
        Ok(())
    }
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Option<T> {
    let bytes = fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn upsert<T, F: Fn(&T) -> &String>(items: &mut Vec<T>, item: T, id_of: F) {
    match items.iter_mut().find(|it| id_of(it) == id_of(&item)) {
        Some(slot) => *slot = item,
        None => items.push(item),
    }
}

/// Seeded on first launch. The first entry ("System default") launches the
/// OS default shell; the rest are common shells detected per platform, à la
/// Windows Terminal's auto-generated profiles.
fn default_profiles() -> Vec<Profile> {
    // Ids are stable strings (not random UUIDs) so the seeded set is
    // consistent across reads before it is ever persisted — the same reason
    // Windows Terminal gives its auto-generated profiles fixed GUIDs.
    let profile = |id: &str, name: &str, command: &str| Profile {
        id: format!("builtin:{id}"),
        name: name.into(),
        command: command.into(),
        args: Vec::new(),
        cwd: String::new(),
    };
    let mut out = vec![profile("default", "システム既定シェル", "")];
    if cfg!(windows) {
        out.push(profile("powershell", "PowerShell", "powershell.exe"));
        out.push(profile("cmd", "コマンド プロンプト", "cmd.exe"));
    } else {
        for (id, name, path) in [
            ("bash", "bash", "/bin/bash"),
            ("zsh", "zsh", "/bin/zsh"),
            ("fish", "fish", "/usr/bin/fish"),
        ] {
            if std::path::Path::new(path).exists() {
                out.push(profile(id, name, path));
            }
        }
    }
    out
}

/// Seeded on first launch so the workflows panel isn't empty.
fn default_workflows() -> Vec<Workflow> {
    let wf = |name: &str, description: &str, command: &str, tags: &[&str]| Workflow {
        id: uuid::Uuid::new_v4().to_string(),
        name: name.into(),
        description: description.into(),
        command: command.into(),
        tags: tags.iter().map(|t| t.to_string()).collect(),
    };
    vec![
        wf(
            "Git log graph",
            "Compact commit graph for the current repo",
            "git log --oneline --graph --decorate -n {{count:20}}",
            &["git"],
        ),
        wf(
            "Find large files",
            "List the biggest files under a directory",
            "du -ah {{path:.}} | sort -rh | head -n {{count:20}}",
            &["disk"],
        ),
        wf(
            "Search in files",
            "Recursive grep with line numbers",
            "grep -rn \"{{pattern}}\" {{path:.}}",
            &["search"],
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::SshAuthMethod;

    fn stores() -> (tempfile::TempDir, Stores) {
        let dir = tempfile::tempdir().unwrap();
        let stores = Stores::new(dir.path()).unwrap();
        (dir, stores)
    }

    #[test]
    fn seeds_default_workflows_and_persists_changes() {
        let (_d, s) = stores();
        let initial = s.list_workflows();
        assert!(!initial.is_empty());

        let saved = s
            .save_workflow(Workflow {
                id: String::new(),
                name: "docker ps".into(),
                description: String::new(),
                command: "docker ps -a".into(),
                tags: vec![],
            })
            .unwrap();
        assert!(!saved.id.is_empty());
        assert!(s.list_workflows().iter().any(|w| w.id == saved.id));

        s.delete_workflow(&saved.id).unwrap();
        assert!(!s.list_workflows().iter().any(|w| w.id == saved.id));
    }

    #[test]
    fn seeds_profiles_and_resolves_default() {
        let (_d, s) = stores();
        let profiles = s.list_profiles();
        assert!(!profiles.is_empty());
        // The first seeded profile launches the OS default shell.
        assert_eq!(profiles[0].command, "");

        // With no default set, resolve falls back to the first profile.
        assert_eq!(s.resolve_profile(None).unwrap().id, profiles[0].id);

        // An explicit request wins over the default.
        let last = profiles.last().unwrap().clone();
        assert_eq!(s.resolve_profile(Some(&last.id)).unwrap().id, last.id);

        // A configured default is honored when nothing is requested.
        let mut settings = s.settings();
        settings.default_profile_id = last.id.clone();
        s.save_settings(&settings).unwrap();
        assert_eq!(s.resolve_profile(None).unwrap().id, last.id);

        // An unknown request id falls through to the default.
        assert_eq!(s.resolve_profile(Some("nope")).unwrap().id, last.id);
    }

    #[test]
    fn profile_crud_roundtrip() {
        let (_d, s) = stores();
        let saved = s
            .save_profile(Profile {
                id: String::new(),
                name: "custom".into(),
                command: "/bin/sh".into(),
                args: vec!["-l".into()],
                cwd: "~/work".into(),
            })
            .unwrap();
        assert!(!saved.id.is_empty());
        assert!(s.list_profiles().iter().any(|p| p.id == saved.id));
        s.delete_profile(&saved.id).unwrap();
        assert!(!s.list_profiles().iter().any(|p| p.id == saved.id));
    }

    #[test]
    fn updates_existing_workflow_in_place() {
        let (_d, s) = stores();
        let mut wf = s.list_workflows().remove(0);
        wf.name = "renamed".into();
        s.save_workflow(wf.clone()).unwrap();
        let listed = s.list_workflows();
        assert_eq!(listed.iter().filter(|w| w.id == wf.id).count(), 1);
        assert_eq!(listed.iter().find(|w| w.id == wf.id).unwrap().name, "renamed");
    }

    #[test]
    fn ssh_hosts_roundtrip() {
        let (_d, s) = stores();
        assert!(s.list_ssh_hosts().is_empty());
        let host = s
            .save_ssh_host(SshHost {
                id: String::new(),
                name: "prod".into(),
                host: "example.com".into(),
                port: 2222,
                username: "deploy".into(),
                auth_method: SshAuthMethod::Key,
                key_path: "~/.ssh/id_ed25519".into(),
            })
            .unwrap();
        let listed = s.list_ssh_hosts();
        assert_eq!(listed, vec![host.clone()]);
        s.delete_ssh_host(&host.id).unwrap();
        assert!(s.list_ssh_hosts().is_empty());
    }

    #[test]
    fn settings_roundtrip() {
        let (_d, s) = stores();
        assert_eq!(s.settings(), Settings::default());
        let new = Settings {
            font_size: 16,
            shell: "/bin/zsh".into(),
            default_profile_id: "abc".into(),
            font_family: "\"JetBrains Mono\", monospace".into(),
            scrollback: 5000,
        };
        s.save_settings(&new).unwrap();
        assert_eq!(s.settings(), new);
    }
}
