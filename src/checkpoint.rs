// fine-grained resume. after each step completes, the runner records the
// (phase, step) cursor here. a failed run leaves the last good cursor on disk,
// so `--resume` skips everything already applied and continues from the next
// step -- never re-running a completed (and possibly destructive) step.

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::Config;

pub const PATH: &str = "/var/lib/raiden/checkpoint.toml";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    pub operation: String,
    pub config_hash: String,
    // index of the last successfully completed step, and the phase it sits in.
    pub phase: usize,
    pub step: usize,
    pub phase_name: String,
}

impl Checkpoint {
    pub fn load(path: &Path) -> Option<Checkpoint> {
        toml::from_str(&std::fs::read_to_string(path).ok()?).ok()
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(path, toml::to_string_pretty(self)?)
            .with_context(|| format!("writing checkpoint {}", path.display()))
    }

    pub fn clear(path: &Path) {
        let _ = std::fs::remove_file(path);
    }
}

/// a stable fingerprint of the resolved config, so resume refuses to continue
/// against a config that changed since the interrupted run.
pub fn config_hash(cfg: &Config) -> String {
    use std::hash::{Hash, Hasher};
    let text = toml::to_string(cfg).unwrap_or_default();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    format!("{:016x}", h.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_load_clear_roundtrip() {
        let path = std::env::temp_dir().join(format!("raiden-cp-test-{}.toml", std::process::id()));
        let cp = Checkpoint {
            operation: "install".into(),
            config_hash: "deadbeef".into(),
            phase: 3,
            step: 5,
            phase_name: "format".into(),
        };
        cp.save(&path).unwrap();
        let back = Checkpoint::load(&path).unwrap();
        assert_eq!(back.operation, "install");
        assert_eq!((back.phase, back.step), (3, 5));
        Checkpoint::clear(&path);
        assert!(Checkpoint::load(&path).is_none());
    }

    #[test]
    fn config_hash_changes_with_config() {
        let mut a = Config::default();
        a.disks.members = vec!["vda".into()];
        let mut b = a.clone();
        b.raid.level = "10".into();
        assert_ne!(config_hash(&a), config_hash(&b));
    }
}
