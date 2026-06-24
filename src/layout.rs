// derived device and mount layout. pure functions of the config; this is the
// typed replacement for the predecessor's options.sh.

use crate::config::Config;

pub const BOOT_MD_NAME: &str = "boot";
pub const BOOT_MD_DEVICE: &str = "/dev/md/boot";
pub const ROOT_MD_NAME: &str = "root";
pub const ROOT_MD_DEVICE: &str = "/dev/md/root";
pub const ZPOOL_NAME: &str = "rpool";

// the primary member's esp is mounted here directly (by uuid). the other members'
// esps are mirrors, resynced from this one via transient mounts under /run/raiden,
// so neither / nor /boot carries a per-disk esp mount point.
pub const ESP_MOUNT: &str = "/boot/efi";

// partition numbers on each member disk.
const PART_ESP: u32 = 1;
const PART_BOOT: u32 = 2;
const PART_ROOT: u32 = 3;

/// where /boot lives: one md raid1 array across all disks, or an independent
/// ext4 /boot per disk kept in sync by a hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootMode {
    Raid,
    Independent,
}

pub struct Layout {
    pub members: Vec<String>,
    part_prefix: String,
    boot_mode: BootMode,
}

impl Layout {
    pub fn derive(cfg: &Config) -> Self {
        Self {
            members: cfg.disks.members.clone(),
            part_prefix: cfg.disks.part_prefix.clone(),
            boot_mode: if cfg.boot.raid {
                BootMode::Raid
            } else {
                BootMode::Independent
            },
        }
    }

    pub fn boot_raid(&self) -> bool {
        self.boot_mode == BootMode::Raid
    }

    /// "/dev/<disk><prefix><n>" for a member's nth partition.
    pub fn part(&self, disk: &str, n: u32) -> String {
        format!("/dev/{}{}{}", disk, self.part_prefix, n)
    }

    fn parts(&self, n: u32) -> Vec<String> {
        self.members.iter().map(|d| self.part(d, n)).collect()
    }

    pub fn esp_devices(&self) -> Vec<String> {
        self.parts(PART_ESP)
    }

    pub fn boot_devices(&self) -> Vec<String> {
        self.parts(PART_BOOT)
    }

    pub fn root_devices(&self) -> Vec<String> {
        self.parts(PART_ROOT)
    }

    /// whether `disk` holds the primary esp (the first member) -- the one mounted
    /// at /boot/efi; the rest are mirrors resynced from it.
    pub fn esp_is_primary(&self, disk: &str) -> bool {
        self.members.first().map(|m| m == disk).unwrap_or(false)
    }

    /// a layout over a subset of the members (eg. the disks being replaced),
    /// keeping the same partition prefix and boot mode.
    pub fn subset(&self, disks: &[String]) -> Layout {
        Layout {
            members: disks.to_vec(),
            part_prefix: self.part_prefix.clone(),
            boot_mode: self.boot_mode,
        }
    }

    /// dm-crypt mapper name for a member's root partition, eg. "vda3_crypt".
    pub fn crypt_name(&self, disk: &str) -> String {
        format!("{}{}{}_crypt", disk, self.part_prefix, PART_ROOT)
    }

    pub fn crypt_names(&self) -> Vec<String> {
        self.members.iter().map(|d| self.crypt_name(d)).collect()
    }

    pub fn crypt_devices(&self) -> Vec<String> {
        self.crypt_names()
            .iter()
            .map(|n| format!("/dev/mapper/{n}"))
            .collect()
    }

    pub fn crypt_device(&self, disk: &str) -> String {
        format!("/dev/mapper/{}", self.crypt_name(disk))
    }

    pub fn int_device_of(&self, disk: &str) -> String {
        format!("/dev/mapper/{}", self.int_name(disk))
    }

    /// dm-integrity mapper name for a member's root partition, eg. "vda3_int".
    pub fn int_name(&self, disk: &str) -> String {
        format!("{}{}{}_int", disk, self.part_prefix, PART_ROOT)
    }

    pub fn int_names(&self) -> Vec<String> {
        self.members.iter().map(|d| self.int_name(d)).collect()
    }

    pub fn int_devices(&self) -> Vec<String> {
        self.int_names()
            .iter()
            .map(|n| format!("/dev/mapper/{n}"))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn layout_with(members: &[&str], prefix: &str) -> Layout {
        let mut c = Config::default();
        c.disks.members = members.iter().map(|s| s.to_string()).collect();
        c.disks.part_prefix = prefix.to_string();
        Layout::derive(&c)
    }

    #[test]
    fn esp_primary_is_the_first_member() {
        let l = layout_with(&["vda", "vdb", "vdc"], "");
        // the primary esp (mounted at /boot/efi) is the first member's p1; the
        // others are mirrors, synced from it.
        assert_eq!(l.esp_devices(), ["/dev/vda1", "/dev/vdb1", "/dev/vdc1"]);
        assert!(l.esp_is_primary("vda"));
        assert!(!l.esp_is_primary("vdb"));
        assert!(!l.boot_raid()); // default config is independent
    }

    #[test]
    fn boot_mode_and_subset_follow_config() {
        let mut c = Config::default();
        c.disks.members = vec!["vda".into(), "vdb".into()];
        c.boot.raid = true;
        let l = Layout::derive(&c);
        assert!(l.boot_raid());
        assert!(l.subset(&["vdb".into()]).boot_raid());
    }

    #[test]
    fn crypt_names_follow_root_partition() {
        let l = layout_with(&["vda", "vdb"], "");
        assert_eq!(l.crypt_names(), ["vda3_crypt", "vdb3_crypt"]);
    }

    #[test]
    fn nvme_prefix_applies_to_partitions() {
        let l = layout_with(&["nvme0n1"], "p");
        assert_eq!(l.part("nvme0n1", 3), "/dev/nvme0n1p3");
        assert_eq!(l.crypt_name("nvme0n1"), "nvme0n1p3_crypt");
    }
}
