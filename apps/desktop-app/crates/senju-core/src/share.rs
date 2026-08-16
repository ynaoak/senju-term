//! Workflow export/import: a small, versioned JSON envelope so users can
//! share workflow sets (e.g. commit them to a team repository) and import
//! them on another machine. Ids are stripped on export and ignored on
//! import — the receiving store assigns fresh ones, so imports can never
//! silently overwrite an existing workflow by id collision.

use serde::{Deserialize, Serialize};

use crate::models::Workflow;

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
