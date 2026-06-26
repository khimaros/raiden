// install-time configuration: typed TOML input merged with environment and flag
// overrides. precedence, lowest to highest: defaults, file, env, flags.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const STACK_MD_LVM_EXT4: &str = "dm-crypt~md~lvm~ext4";
pub const STACK_MD_LVM_XFS: &str = "dm-crypt~md~lvm~xfs";
pub const STACK_BTRFS: &str = "dm-crypt~btrfs";
pub const STACK_BCACHEFS: &str = "dm-crypt~bcachefs";
pub const STACK_ZFS: &str = "dm-crypt~zfs";
pub const STACK_MD_INTEGRITY: &str = "dm-integrity~md~dm-crypt~lvm~ext4";

pub const STACKS: &[&str] = &[
    STACK_MD_LVM_EXT4,
    STACK_MD_LVM_XFS,
    STACK_BTRFS,
    STACK_BCACHEFS,
    STACK_ZFS,
    STACK_MD_INTEGRITY,
];

const MD_LEVELS: &[&str] = &["0", "1", "5", "6", "10"];
const ZFS_LEVELS: &[&str] = &["raidz1", "raidz2", "raidz3"];
// bcachefs redundancy is by replica count, not parity (raid.level is the number
// of data/metadata copies passed to mkfs.bcachefs --replicas).
const BCACHEFS_LEVELS: &[&str] = &["1", "2", "3", "4"];
const BTRFS_LEVELS: &[&str] = &[
    "raid0", "raid1", "raid1c2", "raid1c3", "raid1c4", "raid5", "raid6", "raid10",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    Md,
    Btrfs,
    Zfs,
    Bcachefs,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub install: Install,
    pub disks: Disks,
    pub raid: Raid,
    pub crypt: Crypt,
    pub integrity: Integrity,
    pub btrfs: Btrfs,
    pub boot: Boot,
    pub benchmark: Benchmark,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Install {
    pub release: String,
    pub backports: bool,
    pub boot_mode: String,
    pub extra_packages: Vec<String>,
    // enable a serial console (ttyS0) on the installed system: useful for
    // headless servers and required by the automated vm test harness.
    pub serial_console: bool,
    // bake the raiden binary + manifest into the initrd so `raiden recover` can
    // bring a degraded root online from the rescue shell (default on).
    pub initramfs_recovery: bool,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Disks {
    pub members: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Raid {
    pub stack: String,
    pub level: String,
    pub metadata_level: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Crypt {
    pub cipher: String,
    pub key_size: u32,
    pub sector_size: u32,
    pub integrity: String,
    // skip luksFormat's full-device integrity wipe (aead only). fast, but leaves
    // integrity tags uninitialized until written.
    pub integrity_no_wipe: bool,
    pub extra_args: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Integrity {
    pub algorithm: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Btrfs {
    pub csum: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Benchmark {
    // sysbench fileio working-set size, passes per mode, and per-mode event counts.
    // random writes churn more per event, so they converge in fewer events. these
    // defaults restore raid-explorations' pre-regression sizing (stable p95).
    pub size: String,
    pub passes: u32,
    pub rndwr_events: u64,
    pub seqwr_events: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Boot {
    // put /boot on an md raid1 array across all disks (true), or give each disk an
    // independent ext4 /boot kept in sync by a hook (false, the default). level is
    // only consulted in raid mode; bitmap is also reused by the root arrays.
    pub raid: bool,
    pub level: u32,
    pub bitmap: String,
}

impl Default for Install {
    fn default() -> Self {
        Self {
            release: "forky".into(),
            backports: false,
            boot_mode: "efi".into(),
            extra_packages: Vec::new(),
            serial_console: false,
            initramfs_recovery: true,
        }
    }
}

impl Default for Raid {
    fn default() -> Self {
        Self {
            stack: STACK_MD_LVM_EXT4.into(),
            level: "6".into(),
            metadata_level: String::new(),
        }
    }
}

impl Default for Crypt {
    fn default() -> Self {
        Self {
            cipher: "aegis128-plain64".into(),
            key_size: 128,
            sector_size: 4096,
            integrity: "aead".into(),
            integrity_no_wipe: false,
            extra_args: Vec::new(),
        }
    }
}

impl Default for Integrity {
    fn default() -> Self {
        Self {
            // crc32c is a kernel built-in; xxhash64 needs the xxhash crypto module,
            // which is absent from the live env and the integrity initramfs hook.
            algorithm: "crc32c".into(),
        }
    }
}

impl Default for Btrfs {
    fn default() -> Self {
        Self {
            csum: "xxhash".into(),
        }
    }
}

impl Default for Boot {
    fn default() -> Self {
        Self {
            raid: false,
            level: 1,
            bitmap: "internal".into(),
        }
    }
}

impl Default for Benchmark {
    fn default() -> Self {
        Self {
            size: "2G".into(),
            passes: 3,
            rndwr_events: 5000,
            seqwr_events: 20000,
        }
    }
}

/// overrides sourced from the environment or command line flags, applied on top
/// of the loaded file in that order.
#[derive(Debug, Default, Clone)]
pub struct Overrides {
    pub stack: Option<String>,
    pub level: Option<String>,
    pub disks: Option<Vec<String>>,
    pub release: Option<String>,
}

impl Config {
    /// load from `path`, falling back to built-in defaults when the file does
    /// not exist, then apply environment and flag overrides.
    pub fn load(path: &Path, flags: &Overrides) -> Result<Self> {
        let mut cfg = if path.exists() {
            let text = std::fs::read_to_string(path)
                .with_context(|| format!("reading config {}", path.display()))?;
            toml::from_str(&text).with_context(|| format!("parsing config {}", path.display()))?
        } else {
            Config::default()
        };
        cfg.apply(&Overrides::from_env());
        cfg.apply(flags);
        Ok(cfg)
    }

    /// apply environment then flag overrides, for a config sourced elsewhere
    /// (eg. the install manifest).
    pub fn merge(&mut self, flags: &Overrides) {
        self.apply(&Overrides::from_env());
        self.apply(flags);
    }

    fn apply(&mut self, o: &Overrides) {
        if let Some(v) = &o.stack {
            self.raid.stack = v.clone();
        }
        if let Some(v) = &o.level {
            self.raid.level = v.clone();
        }
        if let Some(v) = &o.disks {
            self.disks.members = v.clone();
        }
        if let Some(v) = &o.release {
            self.install.release = v.clone();
        }
    }

    pub fn family(&self) -> Result<Family> {
        match self.raid.stack.as_str() {
            STACK_MD_LVM_EXT4 | STACK_MD_LVM_XFS | STACK_MD_INTEGRITY => Ok(Family::Md),
            STACK_BTRFS => Ok(Family::Btrfs),
            STACK_BCACHEFS => Ok(Family::Bcachefs),
            STACK_ZFS => Ok(Family::Zfs),
            other => bail!(
                "unknown stack {other:?}; valid stacks: {}",
                STACKS.join(", ")
            ),
        }
    }

    /// metadata level, defaulting to the data level when unset (btrfs only).
    pub fn metadata_level(&self) -> &str {
        if self.raid.metadata_level.is_empty() {
            &self.raid.level
        } else {
            &self.raid.metadata_level
        }
    }

    /// validate without touching any disk.
    pub fn validate(&self) -> Result<()> {
        let family = self.family()?;

        if self.disks.members.is_empty() {
            bail!("disks.members is empty; list at least one member disk");
        }
        let mut seen = std::collections::HashSet::new();
        for d in &self.disks.members {
            if !seen.insert(d) {
                bail!("disks.members contains duplicate {d:?}");
            }
        }

        match self.install.boot_mode.as_str() {
            "efi" | "bios" => {}
            other => bail!("install.boot_mode must be \"efi\" or \"bios\", got {other:?}"),
        }

        match self.crypt.integrity.as_str() {
            "aead" | "none" => {}
            other => bail!("crypt.integrity must be \"aead\" or \"none\", got {other:?}"),
        }

        let (valid, label) = match family {
            Family::Md => (MD_LEVELS, "md"),
            Family::Zfs => (ZFS_LEVELS, "zfs"),
            Family::Btrfs => (BTRFS_LEVELS, "btrfs"),
            Family::Bcachefs => (BCACHEFS_LEVELS, "bcachefs"),
        };
        if !valid.contains(&self.raid.level.as_str()) {
            bail!(
                "raid.level {:?} invalid for {label} stack; valid: {}",
                self.raid.level,
                valid.join(", ")
            );
        }

        if self.benchmark.passes == 0 {
            bail!("benchmark.passes must be at least 1");
        }
        if self.benchmark.rndwr_events == 0 || self.benchmark.seqwr_events == 0 {
            bail!("benchmark.rndwr_events and benchmark.seqwr_events must be non-zero");
        }
        Ok(())
    }
}

impl Overrides {
    pub fn from_env() -> Self {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        Self {
            stack: var("RAIDEN_STACK"),
            level: var("RAIDEN_LEVEL"),
            disks: var("RAIDEN_DISKS").map(|v| split_disks(&v)),
            release: var("RAIDEN_RELEASE"),
        }
    }

    /// overlay `higher`-precedence overrides on top of these, keeping the
    /// defaults, file, env, flags ordering (env overlaid by flags here).
    pub fn overlay(mut self, higher: &Overrides) -> Self {
        if higher.stack.is_some() {
            self.stack = higher.stack.clone();
        }
        if higher.level.is_some() {
            self.level = higher.level.clone();
        }
        if higher.disks.is_some() {
            self.disks = higher.disks.clone();
        }
        if higher.release.is_some() {
            self.release = higher.release.clone();
        }
        self
    }
}

/// split a comma-separated disk list, trimming blanks.
pub fn split_disks(s: &str) -> Vec<String> {
    s.split(',')
        .map(|d| d.trim().to_string())
        .filter(|d| !d.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn md_config() -> Config {
        let mut c = Config::default();
        c.disks.members = vec!["vda".into(), "vdb".into(), "vdc".into(), "vdd".into()];
        c
    }

    #[test]
    fn default_md_config_validates() {
        assert!(md_config().validate().is_ok());
    }

    #[test]
    fn empty_members_rejected() {
        assert!(Config::default().validate().is_err());
    }

    #[test]
    fn duplicate_members_rejected() {
        let mut c = md_config();
        c.disks.members.push("vda".into());
        assert!(c.validate().is_err());
    }

    #[test]
    fn wrong_level_for_family_rejected() {
        let mut c = md_config();
        c.raid.stack = STACK_ZFS.into();
        c.raid.level = "6".into();
        assert!(c.validate().is_err());
        c.raid.level = "raidz2".into();
        assert!(c.validate().is_ok());
    }

    #[test]
    fn metadata_level_defaults_to_level() {
        let mut c = md_config();
        c.raid.level = "raid6".into();
        assert_eq!(c.metadata_level(), "raid6");
        c.raid.metadata_level = "raid1c3".into();
        assert_eq!(c.metadata_level(), "raid1c3");
    }

    #[test]
    fn boot_raid_defaults_off_and_parses() {
        assert!(!Config::default().boot.raid);
        let c: Config = toml::from_str("[boot]\nraid = true\n").unwrap();
        assert!(c.boot.raid);
    }

    #[test]
    fn flags_override_file() {
        let mut c = md_config();
        c.apply(&Overrides {
            level: Some("10".into()),
            ..Default::default()
        });
        assert_eq!(c.raid.level, "10");
    }
}
