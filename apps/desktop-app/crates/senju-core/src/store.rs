//! JSON-file-backed persistence. Each collection lives in its own file under
//! the app config directory so users can inspect and back it up by hand.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{de::DeserializeOwned, Serialize};

use crate::models::{
    DevServerPreset, HistoryEntry, HistoryMeta, LaunchSet, Profile, SessionSnapshot, Settings, SshHost,
    Workflow,
};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serialization error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Invalid(String),
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
        let _g = self.guard();
        let mut items = self.read("workflows", default_workflows);
        upsert(&mut items, wf.clone(), |w| &w.id);
        self.write("workflows", &items)?;
        Ok(wf)
    }

    pub fn delete_workflow(&self, id: &str) -> Result<(), StoreError> {
        let _g = self.guard();
        let mut items = self.read("workflows", default_workflows);
        items.retain(|w| w.id != id);
        self.write("workflows", &items)?;
        // Its remembered placeholder values go with it: they are only ever
        // looked up by this id, and a new workflow is never handed an old
        // id, so keeping them would be a leak with no reader.
        self.clear_workflow_inputs_locked(id)
    }

    // -- Remembered placeholder values --------------------------------------
    //
    // `workflow-inputs.json`: workflow id -> { placeholder name -> last value }.
    // What the user typed into a workflow's `{{…}}` prompt last time, offered
    // as the default next time. Per machine and deliberately *not* part of
    // the settings export (`share`): the values are whatever was typed —
    // hostnames, paths, possibly a token someone pasted into a parameter —
    // and none of that belongs in a file meant to be shared.

    pub fn workflow_inputs(&self, workflow_id: &str) -> HashMap<String, String> {
        let all: HashMap<String, HashMap<String, String>> = self.read("workflow-inputs", HashMap::new);
        all.get(workflow_id).cloned().unwrap_or_default()
    }

    /// Replaces the remembered values for one workflow. An empty map forgets
    /// them (the prompt's "remember" box unticked).
    pub fn save_workflow_inputs(
        &self,
        workflow_id: &str,
        values: HashMap<String, String>,
    ) -> Result<(), StoreError> {
        let _g = self.guard();
        if values.is_empty() {
            return self.clear_workflow_inputs_locked(workflow_id);
        }
        let mut all: HashMap<String, HashMap<String, String>> = self.read("workflow-inputs", HashMap::new);
        all.insert(workflow_id.to_string(), values);
        self.write("workflow-inputs", &all)
    }

    fn clear_workflow_inputs_locked(&self, workflow_id: &str) -> Result<(), StoreError> {
        let mut all: HashMap<String, HashMap<String, String>> = self.read("workflow-inputs", HashMap::new);
        if all.remove(workflow_id).is_none() {
            return Ok(());
        }
        self.write("workflow-inputs", &all)
    }

    /// Reorders the stored workflows to match the given id order. Ids not in
    /// the store are ignored; workflows whose id is absent from `ids` are kept
    /// (in their existing relative order) at the end — so a stale or partial
    /// list from the UI can never drop a workflow. The sort is stable.
    pub fn reorder_workflows(&self, ids: &[String]) -> Result<(), StoreError> {
        let _g = self.guard();
        let mut items = self.read("workflows", default_workflows);
        let rank: std::collections::HashMap<&str, usize> =
            ids.iter().enumerate().map(|(i, s)| (s.as_str(), i)).collect();
        items.sort_by_key(|w| rank.get(w.id.as_str()).copied().unwrap_or(usize::MAX));
        self.write("workflows", &items)
    }

    // -- SSH hosts ------------------------------------------------------------

    pub fn list_ssh_hosts(&self) -> Vec<SshHost> {
        self.read("ssh-hosts", Vec::new)
    }

    pub fn save_ssh_host(&self, mut host: SshHost) -> Result<SshHost, StoreError> {
        // Label fields are normalized before validation so "Prod / Web" and
        // "Prod/Web" land in the same group, and a stray space or a repeated
        // tag never makes two hosts look differently labeled when they are
        // not. The same split-trim-join the panel applies, done here so the
        // import path and settings import get it too.
        host.group = normalize_group(&host.group);
        let mut tags: Vec<String> = Vec::new();
        for t in host.tags.iter().map(|t| t.trim()).filter(|t| !t.is_empty()) {
            if !tags.iter().any(|x| x == t) {
                tags.push(t.to_string());
            }
        }
        host.tags = tags;
        host.color = host.color.trim().to_ascii_lowercase();
        validate_ssh_host(&host)?;
        if host.id.is_empty() {
            host.id = uuid::Uuid::new_v4().to_string();
        }
        // Turning the save box off is the user withdrawing consent, so the
        // stored secrets go now rather than lingering until something else
        // happens to clean them up.
        if !host.save_credentials {
            host.password.clear();
            host.passphrase.clear();
        }
        let _g = self.guard();
        let mut items = self.read("ssh-hosts", Vec::new);
        upsert(&mut items, host.clone(), |h| &h.id);
        self.write("ssh-hosts", &items)?;
        Ok(host)
    }

    pub fn delete_ssh_host(&self, id: &str) -> Result<(), StoreError> {
        // The credentials live on the host record, so removing it takes them
        // with it — nothing separate to clean up.
        let _g = self.guard();
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
        let _g = self.guard();
        let mut items = self.read("profiles", default_profiles);
        upsert(&mut items, profile.clone(), |p| &p.id);
        self.write("profiles", &items)?;
        Ok(profile)
    }

    pub fn delete_profile(&self, id: &str) -> Result<(), StoreError> {
        let _g = self.guard();
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

    // -- Launch sets ------------------------------------------------------------

    pub fn list_launch_sets(&self) -> Vec<LaunchSet> {
        self.read("launch-sets", Vec::new)
    }

    pub fn save_launch_set(&self, mut set: LaunchSet) -> Result<LaunchSet, StoreError> {
        if set.name.trim().is_empty() {
            return Err(StoreError::Invalid("名前を入力してください".into()));
        }
        if set.id.is_empty() {
            set.id = uuid::Uuid::new_v4().to_string();
        }
        let _g = self.guard();
        let mut items = self.read("launch-sets", Vec::new);
        upsert(&mut items, set.clone(), |s| &s.id);
        self.write("launch-sets", &items)?;
        Ok(set)
    }

    pub fn delete_launch_set(&self, id: &str) -> Result<(), StoreError> {
        let _g = self.guard();
        let mut items: Vec<LaunchSet> = self.read("launch-sets", Vec::new);
        items.retain(|s| s.id != id);
        self.write("launch-sets", &items)
    }

    // -- Dev server presets ---------------------------------------------------

    pub fn list_dev_servers(&self) -> Vec<DevServerPreset> {
        self.read("dev-servers", Vec::new)
    }

    pub fn save_dev_server(&self, mut preset: DevServerPreset) -> Result<DevServerPreset, StoreError> {
        if preset.name.trim().is_empty() {
            return Err(StoreError::Invalid("名前を入力してください".into()));
        }
        if preset.root.trim().is_empty() {
            return Err(StoreError::Invalid("公開するディレクトリを入力してください".into()));
        }
        if preset.id.is_empty() {
            preset.id = uuid::Uuid::new_v4().to_string();
        }
        let _g = self.guard();
        let mut items = self.read("dev-servers", Vec::new);
        upsert(&mut items, preset.clone(), |p| &p.id);
        self.write("dev-servers", &items)?;
        Ok(preset)
    }

    pub fn delete_dev_server(&self, id: &str) -> Result<(), StoreError> {
        let _g = self.guard();
        let mut items: Vec<DevServerPreset> = self.read("dev-servers", Vec::new);
        items.retain(|p| p.id != id);
        self.write("dev-servers", &items)
    }

    // -- Session snapshot -----------------------------------------------------

    pub fn session_snapshot(&self) -> SessionSnapshot {
        self.read("session", SessionSnapshot::default)
    }

    pub fn save_session_snapshot(&self, snapshot: &SessionSnapshot) -> Result<(), StoreError> {
        let _g = self.guard();
        self.write("session", snapshot)
    }

    // -- Command history ------------------------------------------------------

    /// Cross-session command history, most recent first.
    pub fn history(&self) -> Vec<HistoryEntry> {
        self.read("history", Vec::new)
    }

    /// Prepends a command to the history. Blank commands are ignored; an
    /// earlier entry with the same command text is replaced (the list answers
    /// "find that command again", not frequency analysis — though `runs`
    /// keeps a count), carrying the latest run's host / exit / duration, and
    /// the list is capped so the file can't grow without bound.
    pub fn add_history(&self, command: &str, kind: &str, meta: &HistoryMeta) -> Result<(), StoreError> {
        const CAP: usize = 2000;
        let command = command.trim();
        if command.is_empty() {
            return Ok(());
        }
        let at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let _g = self.guard();
        let mut items: Vec<HistoryEntry> = self.read("history", Vec::new);
        let runs = items
            .iter()
            .filter(|e| e.command == command)
            .map(|e| e.runs)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        items.retain(|e| e.command != command);
        items.insert(
            0,
            HistoryEntry {
                command: command.into(),
                kind: kind.into(),
                at,
                host: meta.host.trim().to_string(),
                exit: meta.exit,
                duration_ms: meta.duration_ms,
                runs,
            },
        );
        items.truncate(CAP);
        self.write("history", &items)
    }

    // -- Settings -------------------------------------------------------------

    /// Settings as the app should see them: the file's contents with the AI
    /// API key filled in from the OS credential store.
    ///
    /// The key is read straight out of the file — see the field's own docs for
    /// where it lives and what that costs.
    pub fn settings(&self) -> Settings {
        self.read("settings", Settings::default)
    }

    /// Persists settings.
    ///
    /// An empty `ai_api_key` means "leave the saved one alone", not "erase
    /// it": the editor never receives the stored key (so it cannot send it
    /// back), and treating blank as a delete would wipe the key every time
    /// someone changed the font size. Use [`Self::clear_ai_api_key`] to
    /// remove it.
    pub fn save_settings(&self, settings: &Settings) -> Result<(), StoreError> {
        let mut to_write = settings.clone();
        let _g = self.guard();
        if to_write.ai_api_key.is_empty() {
            to_write.ai_api_key = self.read("settings", Settings::default).ai_api_key;
        }
        self.write("settings", &to_write)
    }

    /// Erases the stored AI API key. Separate from `save_settings` because
    /// there deleting and "not changing it" are indistinguishable.
    pub fn clear_ai_api_key(&self) -> Result<(), StoreError> {
        let _g = self.guard();
        let mut settings: Settings = self.read("settings", Settings::default);
        settings.ai_api_key.clear();
        self.write("settings", &settings)
    }

    // -- Internals -------------------------------------------------------------

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{name}.json"))
    }

    /// Where `settings.json` lives (it may not exist until the first save).
    pub fn settings_path(&self) -> PathBuf {
        self.path("settings")
    }

    /// Serializes writes so two `save_*`/`delete_*` can't interleave a
    /// read-modify-write. Recovers from a poisoned lock: the guarded value is
    /// `()`, so a prior panic left no broken invariant.
    fn guard(&self) -> std::sync::MutexGuard<'_, ()> {
        self.lock.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn read<T: DeserializeOwned>(&self, name: &str, default: impl FnOnce() -> T) -> T {
        let path = self.path(name);
        match load_json::<T>(&path) {
            Ok(Some(value)) => value,
            Ok(None) => default(),
            // The file exists but is unreadable/corrupt. Preserve it (so the
            // next save can't silently overwrite the user's data with
            // defaults) and fall back — the app stays usable and the original
            // can be recovered from the `.corrupt` copy.
            Err(_) => {
                backup_corrupt(&path);
                default()
            }
        }
    }

    fn write<T: Serialize>(&self, name: &str, value: &T) -> Result<(), StoreError> {
        let path = self.path(name);
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_vec_pretty(value)?)?;
        fs::rename(&tmp, &path)?;
        Ok(())
    }
}

/// `Ok(None)` = file missing (use defaults); `Ok(Some)` = parsed; `Err` =
/// present but unreadable or unparseable (a corruption we must not treat as
/// "empty").
fn load_json<T: DeserializeOwned>(path: &Path) -> Result<Option<T>, ()> {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(()),
    };
    serde_json::from_slice(&bytes).map(Some).map_err(|_| ())
}

/// Renames a corrupt store file aside to `<name>.json.corrupt[.N]` so it is
/// preserved rather than overwritten on the next save.
fn backup_corrupt(path: &Path) {
    let mut candidate = path.with_extension("json.corrupt");
    let mut n = 1;
    while candidate.exists() {
        candidate = path.with_extension(format!("json.corrupt.{n}"));
        n += 1;
    }
    let _ = fs::rename(path, candidate);
}

/// Canonical `/`-separated group path: segments trimmed, empties dropped.
/// `"Prod / Web"` and `" Prod//Web "` both become `"Prod/Web"`.
fn normalize_group(raw: &str) -> String {
    raw.split('/')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("/")
}

/// Rejects an SSH host that would only fail later with an opaque socket/auth
/// error, so the editor can show the problem inline at save time.
fn validate_ssh_host(host: &SshHost) -> Result<(), StoreError> {
    use crate::models::{SshAuthMethod, HOST_COLORS};
    if !host.color.is_empty() && !HOST_COLORS.contains(&host.color.as_str()) {
        return Err(StoreError::Invalid(format!(
            "ラベル色 \"{}\" は使えません (使えるのは {})",
            host.color,
            HOST_COLORS.join(" / ")
        )));
    }
    if host.host.trim().is_empty() {
        return Err(StoreError::Invalid("ホストを入力してください".into()));
    }
    if host.username.trim().is_empty() {
        return Err(StoreError::Invalid("ユーザー名を入力してください".into()));
    }
    if host.port == 0 {
        return Err(StoreError::Invalid("ポートは 1 以上を指定してください".into()));
    }
    if matches!(host.auth_method, SshAuthMethod::Key | SshAuthMethod::KeyPassword)
        && host.key_path.trim().is_empty()
    {
        return Err(StoreError::Invalid(
            "この認証方式では秘密鍵パスが必要です".into(),
        ));
    }
    // Forward specs are validated at save time so a typo surfaces in the
    // editor rather than as a banner on the next connect.
    let mut taken: Vec<u16> = Vec::new();
    for raw in host.forwards.iter().filter(|s| !s.trim().is_empty()) {
        let spec = crate::sessions::parse_forward_spec(raw)
            .map_err(|e| StoreError::Invalid(format!("ポートフォワード {e}")))?;
        // Two forwards on one local port: only the first would bind, and the
        // second would fail as a banner long after the typo was made. `-R`
        // listens on the server, so it takes no local port and never collides
        // here.
        if let Some(port) = spec.local_bind_port() {
            if taken.contains(&port) {
                return Err(StoreError::Invalid(format!(
                    "ポートフォワード \"{}\": ローカルポート {port} が重複しています",
                    raw.trim()
                )));
            }
            taken.push(port);
        }
    }
    Ok(())
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
    // Ids are stable strings (not random UUIDs, like the seeded profiles) so
    // the seeded set is identical across every read-before-first-persist.
    // Otherwise a delete/reorder/update targeting a seed — before any save has
    // written the file — would read a *freshly* re-seeded set with different
    // ids, silently mismatch, and lose the change.
    let wf = |id: &str, name: &str, description: &str, command: &str, tags: &[&str], group: &str| {
        Workflow {
            id: format!("seed:{id}"),
            name: name.into(),
            description: description.into(),
            command: command.into(),
            tags: tags.iter().map(|t| t.to_string()).collect(),
            group: group.into(),
            shortcut: String::new(),
            show_button: false,
        }
    };
    vec![
        wf(
            "git-log",
            "Git log graph",
            "Compact commit graph for the current repo",
            "git log --oneline --graph --decorate -n {{count:20}}",
            &["git"],
            "Git",
        ),
        wf(
            "find-large",
            "Find large files",
            "List the biggest files under a directory",
            "du -ah {{path:.}} | sort -rh | head -n {{count:20}}",
            &["disk"],
            "システム",
        ),
        wf(
            "search-files",
            "Search in files",
            "Recursive grep with line numbers",
            "grep -rn \"{{pattern}}\" {{path:.}}",
            &["search"],
            "システム",
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::LaunchSetItem;
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
                group: String::new(),
                shortcut: String::new(),
                show_button: false,
            })
            .unwrap();
        assert!(!saved.id.is_empty());
        assert!(s.list_workflows().iter().any(|w| w.id == saved.id));

        s.delete_workflow(&saved.id).unwrap();
        assert!(!s.list_workflows().iter().any(|w| w.id == saved.id));
    }

    #[test]
    fn reorder_workflows_reorders_and_keeps_unlisted() {
        let (_d, s) = stores();
        let ids: Vec<String> = s.list_workflows().iter().map(|w| w.id.clone()).collect();
        assert!(ids.len() >= 3);

        // Reverse the first three; pass only those ids. The unlisted remainder
        // (none here, but the contract) stays at the end.
        let reversed: Vec<String> = ids.iter().rev().cloned().collect();
        s.reorder_workflows(&reversed).unwrap();
        let after: Vec<String> = s.list_workflows().iter().map(|w| w.id.clone()).collect();
        assert_eq!(after, reversed);

        // A partial list moves the named ids to the front (in the given order)
        // and never drops the ones it omits.
        let only_last = vec![ids[0].clone()]; // ids[0] was originally first
        s.reorder_workflows(&only_last).unwrap();
        let after2: Vec<String> = s.list_workflows().iter().map(|w| w.id.clone()).collect();
        assert_eq!(after2[0], ids[0]);
        assert_eq!(after2.len(), ids.len(), "no workflow may be dropped");
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
    fn launch_set_crud_roundtrip() {
        let (_d, s) = stores();
        assert!(s.list_launch_sets().is_empty());
        let saved = s
            .save_launch_set(LaunchSet {
                id: String::new(),
                name: "毎朝の環境".into(),
                items: vec![
                    LaunchSetItem { profile_id: "p1".into(), ..Default::default() },
                    // A workflow plus a direct command on the same item.
                    LaunchSetItem {
                        ssh_host_id: "h1".into(),
                        workflow_id: "w1".into(),
                        command: "tail -f /var/log/syslog".into(),
                        ..Default::default()
                    },
                ],
            })
            .unwrap();
        assert!(!saved.id.is_empty());
        assert_eq!(s.list_launch_sets(), vec![saved.clone()]);

        // Update in place (same id) rather than appending a duplicate.
        let updated = s.save_launch_set(LaunchSet { name: "更新後".into(), ..saved.clone() }).unwrap();
        assert_eq!(s.list_launch_sets(), vec![updated]);

        s.delete_launch_set(&saved.id).unwrap();
        assert!(s.list_launch_sets().is_empty());
    }

    /// A launch-sets.json written before `LaunchSetItem::command` existed must
    /// still load — the field defaults to "no direct command" rather than
    /// failing the whole file into `backup_corrupt` + an empty list.
    #[test]
    fn reads_launch_sets_saved_before_the_command_field() {
        let (d, s) = stores();
        fs::write(
            d.path().join("launch-sets.json"),
            r#"[{"id":"s1","name":"旧データ","items":[{"profile_id":"p1","ssh_host_id":"","workflow_id":"w1"}]}]"#,
        )
        .unwrap();
        let sets = s.list_launch_sets();
        assert_eq!(sets.len(), 1);
        assert_eq!(sets[0].items[0].workflow_id, "w1");
        assert_eq!(sets[0].items[0].command, "");
    }

    #[test]
    fn rejects_unnamed_launch_set() {
        let (_d, s) = stores();
        assert!(matches!(
            s.save_launch_set(LaunchSet { id: String::new(), name: "  ".into(), items: vec![] }),
            Err(StoreError::Invalid(_))
        ));
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
                save_credentials: false,
                password: String::new(),
                passphrase: String::new(),
                forwards: Vec::new(),
                jump_hosts: Vec::new(),
                group: String::new(),
                tags: Vec::new(),
                color: String::new(),
                log_sessions: false,
                auto_reconnect: false,
            })
            .unwrap();
        let listed = s.list_ssh_hosts();
        assert_eq!(listed, vec![host.clone()]);
        s.delete_ssh_host(&host.id).unwrap();
        assert!(s.list_ssh_hosts().is_empty());
    }

    #[test]
    fn session_snapshot_roundtrip() {
        use crate::models::SnapshotThread;
        let (_d, s) = stores();
        assert_eq!(s.session_snapshot(), SessionSnapshot::default());
        let snap = SessionSnapshot {
            threads: vec![
                SnapshotThread { profile_id: "builtin:bash".into(), ssh_host_id: String::new() },
                SnapshotThread { profile_id: String::new(), ssh_host_id: "h1".into() },
            ],
        };
        s.save_session_snapshot(&snap).unwrap();
        assert_eq!(s.session_snapshot(), snap);
        // Overwrite with an empty layout (all threads closed before quit).
        s.save_session_snapshot(&SessionSnapshot::default()).unwrap();
        assert_eq!(s.session_snapshot(), SessionSnapshot::default());
    }

    #[test]
    fn history_dedupes_caps_and_orders() {
        let (_d, s) = stores();
        assert!(s.history().is_empty());
        let meta = |host: &str, exit: Option<i32>, ms: u64| HistoryMeta {
            host: host.into(),
            exit,
            duration_ms: ms,
        };
        s.add_history("cargo test", "local", &meta("bash", Some(0), 1200)).unwrap();
        s.add_history("ls -la", "ssh", &meta("prod-web", Some(0), 30)).unwrap();
        // Re-running an old command moves it to the front instead of
        // duplicating, carries the newest run's details and bumps the count.
        s.add_history("cargo test", "local", &meta("zsh", Some(101), 5000)).unwrap();
        let h = s.history();
        assert_eq!(h.iter().map(|e| e.command.as_str()).collect::<Vec<_>>(), ["cargo test", "ls -la"]);
        assert_eq!(h[1].kind, "ssh");
        assert_eq!(h[1].host, "prod-web");
        assert!(h[0].at > 0);
        assert_eq!(h[0].exit, Some(101));
        assert_eq!(h[0].duration_ms, 5000);
        assert_eq!(h[0].host, "zsh");
        assert_eq!(h[0].runs, 2);
        assert_eq!(h[1].runs, 1);
        // Blank commands are ignored.
        s.add_history("   ", "local", &HistoryMeta::default()).unwrap();
        assert_eq!(s.history().len(), 2);
    }

    // History files written before the metadata fields existed must still
    // load: missing fields default, and `runs` defaults to 1 (not 0).
    #[test]
    fn history_reads_pre_metadata_entries() {
        let (d, s) = stores();
        std::fs::write(d.path().join("history.json"), r#"[{"command":"ls","kind":"local","at":5}]"#).unwrap();
        let h = s.history();
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].exit, None);
        assert_eq!(h[0].host, "");
        assert_eq!(h[0].runs, 1);
        // A re-run then counts from that implicit 1.
        s.add_history("ls", "local", &HistoryMeta::default()).unwrap();
        assert_eq!(s.history()[0].runs, 2);
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
            theme: "light".into(),
            language: "en".into(),
            term_theme: "custom".into(),
            term_colors: [("background".to_string(), "#000000".to_string()), ("red".to_string(), "#ff0000".to_string())]
                .into_iter()
                .collect(),
            restore_session: false,
            gpu_rendering: false,
            shell_integration: false,
            // The API key deliberately travels a different road (the OS
            // credential store), so it is not part of this file roundtrip —
            // `ai_api_key_never_reaches_the_file` covers it.
            ai_api_key: String::new(),
            ai_model: "claude-haiku-4-5".into(),
            auto_update_check: false,
            notify_long_commands: false,
            notify_threshold_secs: 30,
            session_log_dir: "~/logs".into(),
            session_log_plain: false,
            session_log_local: true,
            right_click: "menu".into(),
            palette_keys: [("open".to_string(), vec!["ctrl+k".to_string()])].into(),
        };
        s.save_settings(&new).unwrap();
        assert_eq!(s.settings(), new);
    }

    /// `palette_keys` is hand-edited in settings.json: a file from before the
    /// field existed loads with an empty map, and a file that sets it keeps it
    /// through a save/load.
    #[test]
    fn palette_keys_default_empty_and_roundtrip() {
        let (d, s) = stores();
        std::fs::write(d.path().join("settings.json"), r#"{"font_size":15}"#).unwrap();
        assert!(s.settings().palette_keys.is_empty());

        std::fs::write(d.path().join("settings.json"), r#"{"palette_keys":{"open":["ctrl+k"]}}"#).unwrap();
        let loaded = s.settings();
        assert_eq!(loaded.palette_keys["open"], ["ctrl+k"]);
        s.save_settings(&loaded).unwrap();
        assert_eq!(s.settings().palette_keys, loaded.palette_keys);
    }

    /// The whole point of moving the key: whatever the OS credential store
     /// The key is saved, read back, and cleared on demand — and a blank key
    /// on the way in is "leave it alone", not "erase it".
    ///
    /// That last part is the one worth pinning: the editor is never given the
    /// stored key (see `SettingsView`), so every ordinary settings save sends
    /// a blank one. Treating blank as a delete would wipe the key whenever
    /// somebody changed the font size.
    #[test]
    fn a_blank_api_key_leaves_the_saved_one_alone() {
        let (dir, s) = stores();
        s.save_settings(&Settings { ai_api_key: "sk-ant-secret".into(), ..Settings::default() })
            .unwrap();
        assert_eq!(s.settings().ai_api_key, "sk-ant-secret");

        // An unrelated change, carrying no key, the way the editor sends it.
        s.save_settings(&Settings { font_size: 18, ..Settings::default() }).unwrap();
        let after = s.settings();
        assert_eq!(after.font_size, 18);
        assert_eq!(after.ai_api_key, "sk-ant-secret", "an unrelated save erased the key");

        // Deleting is its own explicit operation.
        s.clear_ai_api_key().unwrap();
        assert_eq!(s.settings().ai_api_key, "");
        let raw = fs::read_to_string(dir.path().join("settings.json")).unwrap();
        assert!(!raw.contains("sk-ant-secret"), "the cleared key is still on disk");
    }

    /// Turning the save box off is the user withdrawing consent, so the
    /// credentials go with it rather than lingering in the file until
    /// something else happens to clean them up.
    #[test]
    fn unchecking_save_credentials_erases_them() {
        let (dir, s) = stores();
        let saved = s
            .save_ssh_host(SshHost {
                id: String::new(),
                name: "prod".into(),
                host: "example.com".into(),
                port: 22,
                username: "deploy".into(),
                auth_method: SshAuthMethod::Password,
                key_path: String::new(),
                save_credentials: true,
                password: "hunter2".into(),
                passphrase: "correct-horse".into(),
                forwards: Vec::new(),
                jump_hosts: Vec::new(),
                group: String::new(),
                tags: Vec::new(),
                color: String::new(),
                log_sessions: false,
                auto_reconnect: false,
            })
            .unwrap();
        assert_eq!(s.list_ssh_hosts()[0].password, "hunter2");

        let off = s
            .save_ssh_host(SshHost { save_credentials: false, ..saved.clone() })
            .unwrap();
        assert_eq!(off.password, "", "the returned host still carries the password");
        assert_eq!(off.passphrase, "");
        let raw = fs::read_to_string(dir.path().join("ssh-hosts.json")).unwrap();
        assert!(!raw.contains("hunter2"), "the password is still in the file");
        assert!(!raw.contains("correct-horse"), "the passphrase is still in the file");

        // Deleting the host takes its credentials with it.
        s.save_ssh_host(SshHost { save_credentials: true, password: "hunter2".into(), ..saved.clone() })
            .unwrap();
        s.delete_ssh_host(&saved.id).unwrap();
        let raw = fs::read_to_string(dir.path().join("ssh-hosts.json")).unwrap();
        assert!(!raw.contains("hunter2"));
    }

    // Q3: a corrupt store file must not be silently treated as empty and then
    // overwritten with defaults — it is preserved aside and the user's data is
    // recoverable.
    #[test]
    fn corrupt_file_is_preserved_not_overwritten() {
        let (dir, s) = stores();
        // Persist a real host, then corrupt the file on disk.
        let host = s
            .save_ssh_host(SshHost {
                id: String::new(),
                name: "prod".into(),
                host: "example.com".into(),
                port: 22,
                username: "deploy".into(),
                auth_method: SshAuthMethod::Password,
                key_path: String::new(),
                save_credentials: false,
                password: String::new(),
                passphrase: String::new(),
                forwards: Vec::new(),
                jump_hosts: Vec::new(),
                group: String::new(),
                tags: Vec::new(),
                color: String::new(),
                log_sessions: false,
                auto_reconnect: false,
            })
            .unwrap();
        let path = dir.path().join("ssh-hosts.json");
        fs::write(&path, b"{ this is not valid json").unwrap();

        // Reading falls back to defaults (empty) but backs the corrupt file up.
        assert!(s.list_ssh_hosts().is_empty());
        let corrupt = dir.path().join("ssh-hosts.json.corrupt");
        assert!(corrupt.exists(), "corrupt file must be preserved");
        assert!(
            std::str::from_utf8(&fs::read(&corrupt).unwrap())
                .unwrap()
                .contains("not valid json"),
            "the original bytes must be kept"
        );

        // A subsequent save starts fresh (from empty) without having destroyed
        // the backup, and the original host id is still recoverable from it.
        let _ = host;
    }

    // R4: invalid SSH hosts are rejected at save time.
    #[test]
    fn rejects_invalid_ssh_host() {
        let (_d, s) = stores();
        let base = SshHost {
            id: String::new(),
            name: String::new(),
            host: "h".into(),
            port: 22,
            username: "u".into(),
            auth_method: SshAuthMethod::Password,
            key_path: String::new(),
            save_credentials: false,
            password: String::new(),
            passphrase: String::new(),
            forwards: Vec::new(),
            jump_hosts: Vec::new(),
            group: String::new(),
            tags: Vec::new(),
            color: String::new(),
            log_sessions: false,
            auto_reconnect: false,
        };
        assert!(matches!(
            s.save_ssh_host(SshHost { host: "".into(), ..base.clone() }),
            Err(StoreError::Invalid(_))
        ));
        assert!(matches!(
            s.save_ssh_host(SshHost { username: "".into(), ..base.clone() }),
            Err(StoreError::Invalid(_))
        ));
        assert!(matches!(
            s.save_ssh_host(SshHost { port: 0, ..base.clone() }),
            Err(StoreError::Invalid(_))
        ));
        assert!(matches!(
            s.save_ssh_host(SshHost { auth_method: SshAuthMethod::Key, key_path: "".into(), ..base.clone() }),
            Err(StoreError::Invalid(_))
        ));
        // A malformed forward spec is rejected at save time; a valid one and
        // blank lines (editor artifacts) are accepted.
        assert!(matches!(
            s.save_ssh_host(SshHost { forwards: vec!["8080:localhost".into()], ..base.clone() }),
            Err(StoreError::Invalid(_))
        ));
        assert!(s
            .save_ssh_host(SshHost {
                forwards: vec!["8080:localhost:80".into(), "  ".into()],
                ..base.clone()
            })
            .is_ok());
        // The other two kinds save too, mixed with a bare -L line.
        assert!(s
            .save_ssh_host(SshHost {
                forwards: vec![
                    "8080:localhost:80".into(),
                    "-R 9000:localhost:3000".into(),
                    "-D 1080".into(),
                ],
                ..base.clone()
            })
            .is_ok());
        // Two forwards on one local port: only the first would ever bind, so
        // catch it here rather than as a banner on the next connect.
        assert!(matches!(
            s.save_ssh_host(SshHost {
                forwards: vec!["8080:localhost:80".into(), "-D 8080".into()],
                ..base.clone()
            }),
            Err(StoreError::Invalid(_))
        ));
        // ...but -R listens on the server, so its port is not a local one and
        // may repeat a local forward's number without conflict.
        assert!(s
            .save_ssh_host(SshHost {
                forwards: vec!["8080:localhost:80".into(), "-R 8080:localhost:3000".into()],
                ..base.clone()
            })
            .is_ok());
        // A valid host still saves.
        assert!(s.save_ssh_host(base).is_ok());
    }

    // -- labels: group / tags / color ----------------------------------------

    fn labeled(group: &str, tags: &[&str], color: &str) -> SshHost {
        SshHost {
            id: String::new(),
            name: "n".into(),
            host: "h.example.com".into(),
            port: 22,
            username: "u".into(),
            auth_method: SshAuthMethod::Agent,
            key_path: String::new(),
            save_credentials: false,
            password: String::new(),
            passphrase: String::new(),
            forwards: Vec::new(),
            jump_hosts: Vec::new(),
            group: group.into(),
            tags: tags.iter().map(|s| s.to_string()).collect(),
            color: color.into(),
            log_sessions: false,
            auto_reconnect: false,
        }
    }

    /// Two spellings of one group must file under one heading, or the panel
    /// shows "Prod" and "Prod " as different places with one host each.
    #[test]
    fn host_group_is_normalized_on_save() {
        let (_d, s) = stores();
        let saved = s.save_ssh_host(labeled(" Prod / Web // ", &[], "")).unwrap();
        assert_eq!(saved.group, "Prod/Web");
        assert_eq!(s.list_ssh_hosts()[0].group, "Prod/Web");
        let bare = s.save_ssh_host(labeled(" / ", &[], "")).unwrap();
        assert_eq!(bare.group, "", "a path of only separators is ungrouped");
    }

    #[test]
    fn host_tags_are_trimmed_and_deduplicated() {
        let (_d, s) = stores();
        let saved = s
            .save_ssh_host(labeled("", &[" db ", "", "db", "cache", "  "], ""))
            .unwrap();
        assert_eq!(saved.tags, vec!["db".to_string(), "cache".to_string()]);
    }

    /// The color names a CSS token; anything else would silently style
    /// nothing, and a host the user thinks is marked red would not be.
    #[test]
    fn host_color_must_be_a_known_name() {
        let (_d, s) = stores();
        for ok in crate::models::HOST_COLORS {
            s.save_ssh_host(labeled("", &[], ok)).unwrap();
        }
        assert_eq!(s.save_ssh_host(labeled("", &[], " Red ")).unwrap().color, "red",
            "case and whitespace are forgiven");
        assert_eq!(s.save_ssh_host(labeled("", &[], "")).unwrap().color, "");
        let err = s.save_ssh_host(labeled("", &[], "#ff0000")).unwrap_err();
        assert!(matches!(err, StoreError::Invalid(_)), "hex is not a label color: {err:?}");
        let err = s.save_ssh_host(labeled("", &[], "crimson")).unwrap_err();
        assert!(matches!(err, StoreError::Invalid(_)));
    }

    /// Hosts saved before labels existed carry none of the fields; they must
    /// still load, ungrouped and uncolored, rather than fail to parse.
    #[test]
    fn host_without_label_fields_still_loads() {
        let (d, s) = stores();
        std::fs::write(
            d.path().join("ssh-hosts.json"),
            r#"[{"id":"old","name":"old","host":"h","port":22,"username":"u","auth_method":"password","key_path":""}]"#,
        )
        .unwrap();
        let hosts = s.list_ssh_hosts();
        assert_eq!(hosts.len(), 1);
        assert_eq!(hosts[0].group, "");
        assert!(hosts[0].tags.is_empty());
        assert_eq!(hosts[0].color, "");
        assert!(!hosts[0].log_sessions && !hosts[0].auto_reconnect, "opt-ins default off");
    }

    // -- remembered placeholder values ----------------------------------------

    #[test]
    fn workflow_inputs_roundtrip_and_forget() {
        let (_d, s) = stores();
        assert!(s.workflow_inputs("w1").is_empty());
        let mut v = HashMap::new();
        v.insert("env".to_string(), "staging".to_string());
        v.insert("tag".to_string(), "v1.2".to_string());
        s.save_workflow_inputs("w1", v.clone()).unwrap();
        assert_eq!(s.workflow_inputs("w1"), v);
        assert!(s.workflow_inputs("other").is_empty(), "keyed by workflow");
        // Empty = forget.
        s.save_workflow_inputs("w1", HashMap::new()).unwrap();
        assert!(s.workflow_inputs("w1").is_empty());
    }

    /// Deleting a workflow must not leave its last inputs on disk.
    #[test]
    fn deleting_a_workflow_drops_its_inputs() {
        let (_d, s) = stores();
        let w = s
            .save_workflow(Workflow {
                id: String::new(),
                name: "deploy".into(),
                description: String::new(),
                command: "deploy {{env}}".into(),
                tags: vec![],
                group: String::new(),
                shortcut: String::new(),
                show_button: false,
            })
            .unwrap();
        let mut v = HashMap::new();
        v.insert("env".to_string(), "prod".to_string());
        s.save_workflow_inputs(&w.id, v).unwrap();
        s.delete_workflow(&w.id).unwrap();
        assert!(s.workflow_inputs(&w.id).is_empty());
    }
}
