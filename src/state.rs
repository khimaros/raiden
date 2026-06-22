// the install manifest: the resolved truth written at install time. it embeds
// the full resolved config so post-install operations (status, scrub, rescue,
// replace, close) resolve from it rather than a hand-maintained config. the
// install pipeline writes it into the target; ops on the running system update
// it via save(). devices are resolved from the config at run time (blkid,
// fstab), so no per-disk identifiers are stored here.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::Config;

pub const DEFAULT_PATH: &str = "/etc/raiden/state.toml";
pub const BOOT_MIRROR_PATH: &str = "/boot/raiden/state.toml";

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct State {
    pub config: Config,
}

impl State {
    pub fn from_config(cfg: &Config) -> Self {
        State {
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
        write_with_dir(DEFAULT_PATH, &text).with_context(|| format!("writing {DEFAULT_PATH}"))?;
        // the /boot mirror is best-effort; /boot may not be mounted yet.
        let _ = write_with_dir(BOOT_MIRROR_PATH, &text);
        Ok(())
    }

    pub fn load() -> Result<State> {
        let text = std::fs::read_to_string(DEFAULT_PATH)
            .with_context(|| format!("reading manifest {DEFAULT_PATH}"))?;
        toml::from_str(&text).with_context(|| format!("parsing manifest {DEFAULT_PATH}"))
    }

    pub fn exists() -> bool {
        std::path::Path::new(DEFAULT_PATH).exists()
    }
}

fn write_with_dir(path: &str, text: &str) -> Result<()> {
    if let Some(parent) = std::path::Path::new(path).parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, text)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_round_trips_the_config() {
        let mut cfg = Config::default();
        cfg.disks.members = vec!["vda".into(), "vdb".into(), "vdc".into()];
        let text = State::from_config(&cfg).to_toml();
        let back: State = toml::from_str(&text).unwrap();
        assert_eq!(back.config.disks.members, cfg.disks.members);
    }
}
