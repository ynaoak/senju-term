//! Multi-hop / ProxyJump chain resolution (`ssh -J`). Turns a target host's
//! `jump_hosts` id list into the ordered list of concrete hosts to connect
//! through. Kept free of russh so the resolution/validation rules are unit
//! testable without a network.

use crate::models::SshHost;

/// Hard cap on chain length (jump hops, excluding the target). Guards against
//  pathological configs and bounds the nested-tunnel depth.
pub const MAX_JUMP_HOPS: usize = 8;

/// Resolves `target.jump_hosts` (ids into `all`) into the ordered connection
/// chain **including the target last**: `[jump1, jump2, ..., target]`. For a
/// direct host (empty `jump_hosts`) this is just `[target]`.
///
/// The jump list is taken literally — a jump host's *own* `jump_hosts` are not
/// expanded, so the user specifies the full path explicitly and the result is
/// acyclic by construction. Still validated for: unknown ids, the target
/// appearing as its own jump, duplicate hops, and the hop cap.
pub fn resolve_jump_chain(target: &SshHost, all: &[SshHost]) -> Result<Vec<SshHost>, String> {
    if target.jump_hosts.is_empty() {
        return Ok(vec![target.clone()]);
    }
    if target.jump_hosts.len() > MAX_JUMP_HOPS {
        return Err(format!(
            "踏み台は最大 {MAX_JUMP_HOPS} 段までです(指定: {} 段)",
            target.jump_hosts.len()
        ));
    }
    let mut chain = Vec::with_capacity(target.jump_hosts.len() + 1);
    let mut seen: Vec<&str> = Vec::new();
    for id in &target.jump_hosts {
        if id == &target.id {
            return Err("踏み台に接続先自身を指定することはできません".into());
        }
        if seen.contains(&id.as_str()) {
            return Err("同じ踏み台が複数回指定されています".into());
        }
        let hop = all
            .iter()
            .find(|h| &h.id == id)
            .ok_or_else(|| format!("踏み台に指定された SSH ホストが見つかりません (id={id})"))?;
        seen.push(id.as_str());
        chain.push(hop.clone());
    }
    chain.push(target.clone());
    Ok(chain)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{SshAuthMethod, SshHost};

    fn host(id: &str, jumps: &[&str]) -> SshHost {
        SshHost {
            id: id.into(),
            name: id.into(),
            host: format!("{id}.example.com"),
            port: 22,
            username: "u".into(),
            auth_method: SshAuthMethod::Agent,
            key_path: String::new(),
            forwards: Vec::new(),
            jump_hosts: jumps.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn direct_host_is_single_element_chain() {
        let t = host("target", &[]);
        let chain = resolve_jump_chain(&t, std::slice::from_ref(&t)).unwrap();
        assert_eq!(chain.len(), 1);
        assert_eq!(chain[0].id, "target");
    }

    #[test]
    fn resolves_ordered_chain_with_target_last() {
        let j1 = host("bastion1", &[]);
        let j2 = host("bastion2", &[]);
        let t = host("target", &["bastion1", "bastion2"]);
        let all = vec![j1, j2, t.clone()];
        let chain = resolve_jump_chain(&t, &all).unwrap();
        let ids: Vec<&str> = chain.iter().map(|h| h.id.as_str()).collect();
        assert_eq!(ids, vec!["bastion1", "bastion2", "target"]);
    }

    #[test]
    fn rejects_unknown_duplicate_self_and_overlong() {
        let j1 = host("bastion1", &[]);
        let t = host("target", &["missing"]);
        assert!(resolve_jump_chain(&t, &[j1.clone(), t.clone()]).is_err());

        let dup = host("target", &["bastion1", "bastion1"]);
        assert!(resolve_jump_chain(&dup, &[j1.clone(), dup.clone()]).is_err());

        let sref = host("target", &["target"]);
        assert!(resolve_jump_chain(&sref, std::slice::from_ref(&sref)).is_err());

        let many: Vec<&str> = (0..=MAX_JUMP_HOPS).map(|_| "bastion1").collect();
        let over = host("target", &many);
        // Too many is rejected before the duplicate check even runs.
        assert!(resolve_jump_chain(&over, &[j1, over.clone()]).is_err());
    }
}
