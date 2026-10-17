//! Workflow export/import: a small, versioned JSON envelope so users can
//! share workflow sets (e.g. commit them to a team repository) and import
//! them on another machine. Ids are stripped on export and ignored on
//! import — the receiving store assigns fresh ones, so imports can never
//! silently overwrite an existing workflow by id collision.

use serde::{Deserialize, Serialize};

use crate::models::{DevServerPreset, LaunchSet, Profile, Settings, SshHost, Workflow};

/// Current envelope version. Bump only for incompatible changes; unknown
/// FIELDS are already tolerated (serde defaults), so additive changes don't
/// need a bump.
pub const EXPORT_VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
pub struct WorkflowExport {
    pub version: u32,
    pub workflows: Vec<Workflow>,
}

/// Serializes workflows into the shareable envelope (pretty-printed for
/// readable diffs in version control). Ids are stripped.
pub fn to_export_json(workflows: &[Workflow]) -> String {
    let export = WorkflowExport {
        version: EXPORT_VERSION,
        workflows: workflows
            .iter()
            .cloned()
            .map(|mut w| {
                w.id = String::new();
                w
            })
            .collect(),
    };
    // Serialization of this plain data cannot fail.
    serde_json::to_string_pretty(&export).expect("workflow export serialization")
}

/// Parses an export file back into workflows. Rejects unknown (newer)
/// versions instead of guessing, and blank-named or commandless entries
/// (they could only produce broken cards). Ids are cleared so the store
/// assigns fresh ones on save.
pub fn from_export_json(json: &str) -> Result<Vec<Workflow>, String> {
    let export: WorkflowExport =
        serde_json::from_str(json).map_err(|e| format!("invalid workflow export: {e}"))?;
    if export.version > EXPORT_VERSION {
        return Err(format!(
            "unsupported export version {} (this app supports up to {})",
            export.version, EXPORT_VERSION
        ));
    }
    Ok(export
        .workflows
        .into_iter()
        .filter(|w| !w.name.trim().is_empty() && !w.command.trim().is_empty())
        .map(|mut w| {
            w.id = String::new();
            w
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wf(name: &str, command: &str) -> Workflow {
        Workflow {
            id: "some-id".into(),
            name: name.into(),
            description: "d".into(),
            command: command.into(),
            tags: vec!["t".into()],
            group: "G/H".into(),
            shortcut: "ctrl+alt+x".into(),
            show_button: true,
        }
    }

    #[test]
    fn roundtrip_preserves_fields_but_strips_ids() {
        let json = to_export_json(&[wf("deploy", "deploy {{env}}")]);
        assert!(json.contains("\"version\": 1"));
        let back = from_export_json(&json).unwrap();
        assert_eq!(back.len(), 1);
        let w = &back[0];
        assert_eq!(w.id, "");
        assert_eq!(w.name, "deploy");
        assert_eq!(w.command, "deploy {{env}}");
        assert_eq!(w.group, "G/H");
        assert_eq!(w.shortcut, "ctrl+alt+x");
        assert!(w.show_button);
    }

    #[test]
    fn rejects_newer_versions_and_garbage() {
        assert!(from_export_json("{\"version\": 99, \"workflows\": []}").is_err());
        assert!(from_export_json("not json").is_err());
        assert!(from_export_json("{\"workflows\": []}").is_err()); // version required
    }

    #[test]
    fn drops_unusable_entries() {
        let json = r#"{"version":1,"workflows":[
            {"name":"ok","command":"echo hi"},
            {"name":"  ","command":"echo"},
            {"name":"no-command","command":""}
        ]}"#;
        let back = from_export_json(json).unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].name, "ok");
    }
}

/* ---------------- whole-configuration export ---------------- */

/// Everything the app persists that describes *how it is set up*, in one
/// versioned envelope, so a setup can be moved to another machine or kept in
/// version control.
///
/// **Credentials are not in here.** [`to_config_json`] strips the AI API key
/// and every saved SSH password/passphrase before serializing, so an export
/// can be shared, committed or backed up without carrying secrets — which is
/// the whole reason the settings live in files the user can copy rather than
/// in an OS credential store. Importing therefore always leaves the user
/// re-entering those, by design.
///
/// Deliberately absent: command history (a log of what was typed, not
/// configuration), the session snapshot and window geometry (specific to one
/// machine's screen).
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigExport {
    pub version: u32,
    #[serde(default)]
    pub settings: Option<Settings>,
    #[serde(default)]
    pub workflows: Vec<Workflow>,
    #[serde(default)]
    pub ssh_hosts: Vec<SshHost>,
    #[serde(default)]
    pub profiles: Vec<Profile>,
    #[serde(default)]
    pub launch_sets: Vec<LaunchSet>,
    #[serde(default)]
    pub dev_servers: Vec<DevServerPreset>,
}

/// Builds the export envelope with every secret removed.
///
/// Ids are kept here, unlike the workflow-only export: this file is a whole
/// setup being restored, and keeping ids is what lets a launch set still point
/// at its profile and a jump chain still point at its hosts. Import upserts by
/// id for the same reason.
pub fn to_config_json(
    settings: &Settings,
    workflows: &[Workflow],
    ssh_hosts: &[SshHost],
    profiles: &[Profile],
    launch_sets: &[LaunchSet],
    dev_servers: &[DevServerPreset],
) -> String {
    let mut settings = settings.clone();
    settings.ai_api_key = String::new();
    let export = ConfigExport {
        version: EXPORT_VERSION,
        settings: Some(settings),
        workflows: workflows.to_vec(),
        ssh_hosts: ssh_hosts
            .iter()
            .cloned()
            .map(|mut h| {
                // Both the values and the opt-in flag: arriving with the box
                // already ticked but nothing behind it would show a host that
                // claims to have a saved password and then asks anyway.
                h.password = String::new();
                h.passphrase = String::new();
                h.save_credentials = false;
                h
            })
            .collect(),
        profiles: profiles.to_vec(),
        launch_sets: launch_sets.to_vec(),
        dev_servers: dev_servers.to_vec(),
    };
    serde_json::to_string_pretty(&export).expect("config export serialization")
}

/// Parses a settings-export file. Rejects unknown (newer) versions instead of
/// guessing, and strips credentials again on the way in — a hand-edited file,
/// or one written by some future version, does not get to smuggle them past
/// the promise the export format makes.
pub fn from_config_json(json: &str) -> Result<ConfigExport, String> {
    let mut export: ConfigExport =
        serde_json::from_str(json).map_err(|e| format!("invalid settings export: {e}"))?;
    if export.version > EXPORT_VERSION {
        return Err(format!(
            "unsupported export version {} (this app supports up to {})",
            export.version, EXPORT_VERSION
        ));
    }
    if let Some(settings) = export.settings.as_mut() {
        settings.ai_api_key = String::new();
    }
    for host in &mut export.ssh_hosts {
        host.password = String::new();
        host.passphrase = String::new();
        host.save_credentials = false;
    }
    export.workflows.retain(|w| !w.name.trim().is_empty() && !w.command.trim().is_empty());
    Ok(export)
}

#[cfg(test)]
mod config_tests {
    use super::*;
    use crate::models::SshAuthMethod;

    fn host_with_secrets() -> SshHost {
        SshHost {
            id: "h1".into(),
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
            group: "Prod/Web".into(),
            tags: vec!["db".into(), "pci".into()],
            color: "red".into(),
            log_sessions: false,
            auto_reconnect: false,
        }
    }

    /// Labels are organisation, not secrets: a red "prod" marking is exactly
    /// the kind of thing a team wants to carry to the next machine, so the
    /// export keeps them while stripping the credentials beside them.
    #[test]
    fn export_keeps_group_tags_and_color() {
        let json = to_config_json(&Settings::default(), &[], &[host_with_secrets()], &[], &[], &[]);
        let export = from_config_json(&json).unwrap();
        let h = &export.ssh_hosts[0];
        assert_eq!(h.group, "Prod/Web");
        assert_eq!(h.tags, vec!["db".to_string(), "pci".to_string()]);
        assert_eq!(h.color, "red");
        assert!(h.password.is_empty() && h.passphrase.is_empty(), "secrets still stripped");
    }

    /// The reason this format exists: a settings file the user can share,
    /// commit or back up. If a secret can ride along, the whole trade this
    /// made (files instead of an OS credential store) was a bad one.
    #[test]
    fn the_export_carries_no_secrets() {
        let settings = Settings { ai_api_key: "sk-ant-secret".into(), ..Settings::default() };
        let json = to_config_json(&settings, &[], &[host_with_secrets()], &[], &[], &[]);

        assert!(!json.contains("sk-ant-secret"), "the API key is in the export");
        assert!(!json.contains("hunter2"), "an SSH password is in the export");
        assert!(!json.contains("correct-horse"), "an SSH passphrase is in the export");
        // The host itself is still there — it is configuration.
        assert!(json.contains("example.com"));
        // And it does not arrive claiming to have credentials it lacks.
        assert!(json.contains("\"save_credentials\": false"), "{json}");
    }

    /// A hand-edited file does not get to smuggle credentials past the format's
    /// promise: import strips them again rather than trusting the file.
    #[test]
    fn import_strips_secrets_a_file_tried_to_carry() {
        let smuggled = r#"{
          "version": 1,
          "settings": { "ai_api_key": "sk-ant-smuggled" },
          "sshHosts": [{
            "id": "h1", "name": "prod", "host": "example.com", "port": 22,
            "username": "deploy", "auth_method": "password",
            "save_credentials": true, "password": "hunter2", "passphrase": "correct-horse"
          }]
        }"#;
        let export = from_config_json(smuggled).unwrap();
        assert_eq!(export.settings.unwrap().ai_api_key, "");
        let host = &export.ssh_hosts[0];
        assert_eq!(host.password, "");
        assert_eq!(host.passphrase, "");
        assert!(!host.save_credentials);
        assert_eq!(host.host, "example.com", "the rest of the host must survive");
    }

    #[test]
    fn a_newer_version_is_refused_rather_than_guessed_at() {
        let future = format!(r#"{{ "version": {} }}"#, EXPORT_VERSION + 1);
        assert!(from_config_json(&future).is_err());
        // The current version, with everything absent, is a valid empty export.
        let empty = format!(r#"{{ "version": {EXPORT_VERSION} }}"#);
        let export = from_config_json(&empty).unwrap();
        assert!(export.settings.is_none() && export.ssh_hosts.is_empty());
    }

    /// Ids survive the round trip. A launch set points at a profile by id and a
    /// jump chain points at hosts by id; stripping ids the way the workflow
    /// export does would quietly break both on import.
    #[test]
    fn ids_survive_so_cross_references_still_resolve() {
        let profile = Profile {
            id: "p1".into(),
            name: "zsh".into(),
            command: String::new(),
            args: Vec::new(),
            cwd: String::new(),
        };
        let set = LaunchSet {
            id: "s1".into(),
            name: "morning".into(),
            items: vec![crate::models::LaunchSetItem {
                profile_id: "p1".into(),
                ..Default::default()
            }],
        };
        let json = to_config_json(&Settings::default(), &[], &[], &[profile], &[set], &[]);
        let export = from_config_json(&json).unwrap();
        assert_eq!(export.profiles[0].id, "p1");
        assert_eq!(export.launch_sets[0].items[0].profile_id, "p1");
    }
}
