// derived device and mount layout. pure functions of the config; this is the
// typed replacement for the predecessor's options.sh.

use crate::config::Config;

pub const BOOT_MD_NAME: &str = "boot";
pub const BOOT_MD_DEVICE: &str = "/dev/md/boot";
pub const ROOT_MD_NAME: &str = "root";
pub const ROOT_MD_DEVICE: &str = "/dev/md/root";
pub const ZPOOL_NAME: &str = "rpool";

// /boot/efi is a symlink to the active esp's per-slot mount; grub-install and the
// mirror hook use this one stable path, and a dead primary is failed over by
// re-pointing the link rather than editing fstab.
pub const ESP_LINK: &str = "/boot/efi";

// the live /boot mount. in independent mode the other disks' /boot copies mount
// noauto at /boot.mirrorN (siblings, so the sync rsync --one-file-system never
// recurses into the nested esp mounts or a mirror into itself).
pub const BOOT_MOUNT: &str = "/boot";
const BOOT_MIRROR_PREFIX: &str = "/boot.mirror";

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

/// stable per-slot /boot mount for the disk at the given member index (independent
/// mode). slot 0 is the live primary (/boot); the rest are /boot.mirrorN mirrors.
fn boot_mount(i: usize) -> String {
    if i == 0 {
        BOOT_MOUNT.to_string()
    } else {
        format!("{}{}", BOOT_MIRROR_PREFIX, i + 1)
    }
}

/// stable per-slot esp mount point for the disk at the given member index. every
/// disk's esp has its own /boot/efiN (1-indexed); /boot/efi (ESP_LINK) is a
/// symlink to whichever slot is the current primary.
fn esp_mount(i: usize) -> String {
    format!("/boot/efi{}", i + 1)
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

    /// per-slot esp mount points (/boot/efi1../boot/efiN). positional, so a disk
    /// maps to the same slot every run; the first is the primary (auto), the rest
    /// are noauto mirrors. /boot/efi (ESP_LINK) symlinks to the primary.
    pub fn esp_mounts(&self) -> Vec<String> {
        (0..self.members.len()).map(esp_mount).collect()
    }

    /// the primary esp mount: the symlink target of /boot/efi by default.
    pub fn esp_primary(&self) -> String {
        esp_mount(0)
    }

    /// esp mount slot for a specific disk, by its position in the full member
    /// list -- so replace targets the same slot it had at install.
    pub fn esp_mount_of(&self, disk: &str) -> Option<String> {
        self.members.iter().position(|m| m == disk).map(esp_mount)
    }

    /// per-slot /boot mount points (independent mode): [/boot, /boot.mirror2, ...].
    /// the first is the live primary (auto), the rest are noauto mirrors.
    pub fn boot_mounts(&self) -> Vec<String> {
        (0..self.members.len()).map(boot_mount).collect()
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
    fn esp_mounts_are_stable_per_slot() {
        let l = layout_with(&["vda", "vdb", "vdc"], "");
        assert_eq!(l.esp_mounts(), ["/boot/efi1", "/boot/efi2", "/boot/efi3"]);
        assert_eq!(l.esp_primary(), "/boot/efi1");
        assert_eq!(l.esp_mount_of("vdb").as_deref(), Some("/boot/efi2"));
    }

    #[test]
    fn boot_mounts_put_primary_at_boot_then_mirrors() {
        let l = layout_with(&["vda", "vdb", "vdc"], "");
        assert!(!l.boot_raid()); // default config is independent
        assert_eq!(l.boot_mounts(), ["/boot", "/boot.mirror2", "/boot.mirror3"]);
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
