// the install manifest: the resolved truth written at install time. it embeds
// the full resolved config so post-install operations (status, scrub, rescue,
// replace, close) resolve from it rather than a hand-maintained config. the
// install pipeline writes it into the target; ops on the running system update
// it via save(). devices are resolved from the config at run time (blkid,
// fstab), so no per-disk identifiers are stored here.
//
// /boot/raiden/manifest.toml is the canonical copy: it is reachable from a
// livecd by mounting a member's /boot alone, without unlocking the root fs.
// /etc/raiden/manifest.toml is the mirror, present on the running system. load
// tries /boot first, then /etc; save writes both, best-effort on /boot (which
// may not be mounted during install before the bind phase).

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::Config;

pub const DEFAULT_PATH: &str = "/etc/raiden/manifest.toml";
pub const BOOT_MIRROR_PATH: &str = "/boot/raiden/manifest.toml";

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Manifest {
    pub config: Config,
}

impl Manifest {
    pub fn from_config(cfg: &Config) -> Self {
        Manifest {
            config: cfg.clone(),
        }
    }

    /// serialize the manifest to toml (used both to save() and to emit the
    /// install-time write step).
    pub fn to_toml(&self) -> String {
        toml::to_string_pretty(self).unwrap_or_default()
    }

    pub fn save(&self) -> Result<()> {
        let text = self.to_toml();
        write_atomic(DEFAULT_PATH, &text).with_context(|| format!("writing {DEFAULT_PATH}"))?;
        // the /boot mirror is best-effort; /boot may not be mounted yet.
        let _ = write_atomic(BOOT_MIRROR_PATH, &text);
        Ok(())
    }

    pub fn load() -> Result<Manifest> {
        // /boot first (canonical, livecd-reachable), then /etc.
        for path in [BOOT_MIRROR_PATH, DEFAULT_PATH] {
            if let Ok(text) = std::fs::read_to_string(path) {
                return toml::from_str(&text).with_context(|| format!("parsing manifest {path}"));
            }
        }
        Err(anyhow::anyhow!(
            "no install manifest found at {BOOT_MIRROR_PATH} or {DEFAULT_PATH}; \
             if upgrading from an older raiden, rename state.toml to manifest.toml"
        ))
    }

    pub fn exists() -> bool {
        std::path::Path::new(BOOT_MIRROR_PATH).exists()
            || std::path::Path::new(DEFAULT_PATH).exists()
    }
}

/// write to a temp file in the same dir, then rename over the target. a direct
/// write would leave a truncated manifest visible to a concurrent reader (or a
/// crash mid-write) -- rename is atomic on the same filesystem.
fn write_atomic(path: &str, text: &str) -> Result<()> {
    let p = std::path::Path::new(path);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = p.with_extension("toml.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, p)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_round_trips_the_config() {
        let mut cfg = Config::default();
        cfg.disks.members = vec!["vda".into(), "vdb".into(), "vdc".into()];
        let text = Manifest::from_config(&cfg).to_toml();
        let back: Manifest = toml::from_str(&text).unwrap();
        assert_eq!(back.config.disks.members, cfg.disks.members);
    }
}
