// derived device and mount layout. pure functions of the config (plus resolving
// stable member ids to their current kernel nodes); this is the typed
// replacement for the predecessor's options.sh.

use std::collections::BTreeMap;

use anyhow::{anyhow, bail, Result};

use crate::config::{Config, CryptNames};
use crate::disk_id;

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

// mapper name prefix for `disks.crypt_names = "index"` (raiden0_crypt, ...).
const CRYPT_INDEX_PREFIX: &str = "raiden";

/// "<disk><sep><n>": the kernel partition name. the separator is derived per
/// disk: "p" when the disk name ends in a digit (nvme0n1 -> nvme0n1p1, mmcblk0 ->
/// mmcblk0p1), empty otherwise (sda -> sda1), so one array can mix nvme and sd
/// members, which one global prefix could not. pure.
fn kernel_part(disk: &str, n: u32) -> String {
    let sep = if disk.chars().last().is_some_and(|c| c.is_ascii_digit()) {
        "p"
    } else {
        ""
    };
    format!("{disk}{sep}{n}")
}

/// where /boot lives: one md raid1 array across all disks, or an independent
/// ext4 /boot per disk kept in sync by a hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootMode {
    Raid,
    Independent,
}

pub struct Layout {
    /// member labels (the install-time kernel names), the keys every other
    /// method takes. a subset layout iterates only some of them.
    pub members: Vec<String>,
    /// every member label in config order; index mapper names are positions here.
    slots: Vec<String>,
    /// labels swapped out by `replace --with`, and the slot they held.
    retired: Vec<(String, usize)>,
    /// label ->the kernel name its stable id resolves to now, or the id path
    /// itself when the disk is absent (so it never aliases another disk).
    nodes: BTreeMap<String, String>,
    crypt_names: CryptNames,
    boot_mode: BootMode,
}

impl Layout {
    /// derive the layout, resolving each member's stable id to its current
    /// kernel node (impure only when ids are configured).
    pub fn derive(cfg: &Config) -> Self {
        Self::derive_with(cfg, disk_id::kernel_name_of)
    }

    /// derive with an injected id resolver. pure given a pure `resolve`.
    pub fn derive_with(cfg: &Config, resolve: impl Fn(&str) -> Option<String>) -> Self {
        let nodes = cfg
            .disks
            .ids
            .iter()
            .map(|(label, id)| (label.clone(), resolve(id).unwrap_or_else(|| id.clone())))
            .collect();
        Self {
            members: cfg.disks.members.clone(),
            slots: cfg.disks.members.clone(),
            retired: Vec::new(),
            nodes,
            crypt_names: cfg.disks.crypt_names,
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

    /// the node a member is reachable at now: its resolved kernel name, its
    /// unresolved id path, or (no id recorded) the label itself.
    pub fn node<'a>(&'a self, disk: &'a str) -> &'a str {
        self.nodes.get(disk).map(String::as_str).unwrap_or(disk)
    }

    /// the whole-disk device path of a member as it is named now.
    pub fn disk(&self, disk: &str) -> String {
        let node = self.node(disk);
        if node.starts_with('/') {
            node.to_string()
        } else {
            format!("/dev/{node}")
        }
    }

    /// a member's nth partition as it is named now. an unresolved id path takes
    /// udev's "-part<n>" suffix; a kernel name takes the kernel separator.
    pub fn part(&self, disk: &str, n: u32) -> String {
        let node = self.node(disk);
        if node.starts_with('/') {
            format!("{node}-part{n}")
        } else {
            format!("/dev/{}", kernel_part(node, n))
        }
    }

    /// the member a --disks argument names: a label, a member's current kernel
    /// name, or its stable id path. a label that is now ANOTHER member's kernel
    /// name is refused, since either reading may be the wrong physical disk.
    pub fn member_for(&self, arg: &str) -> Result<String> {
        let name = arg.strip_prefix("/dev/").unwrap_or(arg);
        let by_node = self.slots.iter().find(|m| self.node(m) == name);
        if self.slots.iter().any(|m| m == name) {
            if let Some(other) = by_node.filter(|m| *m != name) {
                bail!(
                    "{arg:?} is ambiguous: it is member {name}'s label but member {other}'s \
                     current kernel name; name the disk by its /dev/disk/by-id path"
                );
            }
            return Ok(name.to_string());
        }
        if let Some(m) = by_node {
            return Ok(m.clone());
        }
        let resolved = disk_id::kernel_name_of(arg);
        self.slots
            .iter()
            .find(|m| {
                self.nodes.get(*m).map(String::as_str) == Some(arg)
                    || (resolved.is_some() && resolved.as_deref() == Some(self.node(m)))
            })
            .cloned()
            .ok_or_else(|| anyhow!("{arg:?} is not a configured member disk"))
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
    /// keeping the same boot mode.
    pub fn subset(&self, disks: &[String]) -> Layout {
        Layout {
            members: disks.to_vec(),
            slots: self.slots.clone(),
            retired: self.retired.clone(),
            nodes: self.nodes.clone(),
            crypt_names: self.crypt_names,
            boot_mode: self.boot_mode,
        }
    }

    /// after a `replace --with` swap, let each outgoing label name the slot its
    /// replacement now holds, so index mapper names of the old disk still resolve.
    pub fn with_retired(&self, pairs: &[(String, String)]) -> Layout {
        let mut l = self.subset(&self.members);
        for (old, new) in pairs {
            if let Some(i) = l.slots.iter().position(|m| m == new) {
                l.retired.push((old.clone(), i));
            }
        }
        l
    }

    /// dm-crypt mapper name for a member's root partition. by default the frozen
    /// install-time partition name ("nvme0n1p3_crypt"), so crypttab stays valid
    /// however the kernel renames the disk; "index" mode uses the member's slot.
    pub fn crypt_name(&self, disk: &str) -> String {
        match self.crypt_names {
            CryptNames::Kernel => format!("{}_crypt", kernel_part(disk, PART_ROOT)),
            CryptNames::Index => format!("{CRYPT_INDEX_PREFIX}{}_crypt", self.slot(disk)),
        }
    }

    /// a member's position in config order (or, for a retired swap label, its
    /// replacement's).
    fn slot(&self, disk: &str) -> usize {
        self.slots
            .iter()
            .position(|m| m == disk)
            .or_else(|| {
                self.retired
                    .iter()
                    .find(|(o, _)| o == disk)
                    .map(|(_, i)| *i)
            })
            .unwrap_or(self.slots.len())
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

    /// dm-integrity mapper name for a member's root partition, eg. "vda3_int". it
    /// follows the CURRENT partition name: the boot-time udev rule opens each
    /// integrity device as "%k_int", so that is the name it actually has.
    pub fn int_name(&self, disk: &str) -> String {
        let p = self.part(disk, PART_ROOT);
        format!("{}_int", p.rsplit('/').next().unwrap_or(&p))
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

    fn layout_with(members: &[&str]) -> Layout {
        let mut c = Config::default();
        c.disks.members = members.iter().map(|s| s.to_string()).collect();
        Layout::derive(&c)
    }

    #[test]
    fn esp_primary_is_the_first_member() {
        let l = layout_with(&["vda", "vdb", "vdc"]);
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
        let l = layout_with(&["vda", "vdb"]);
        assert_eq!(l.crypt_names(), ["vda3_crypt", "vdb3_crypt"]);
    }

    #[test]
    fn nvme_prefix_applies_to_partitions() {
        let l = layout_with(&["nvme0n1"]);
        assert_eq!(l.part("nvme0n1", 3), "/dev/nvme0n1p3");
        assert_eq!(l.crypt_name("nvme0n1"), "nvme0n1p3_crypt");
    }

    #[test]
    fn mixed_nvme_and_sd_members_get_per_disk_prefixes() {
        // a single global prefix was broken for mixed arrays; each disk now
        // derives its own separator.
        let l = layout_with(&["sda", "nvme0n1"]);
        assert_eq!(l.part("sda", 1), "/dev/sda1");
        assert_eq!(l.part("nvme0n1", 1), "/dev/nvme0n1p1");
        assert_eq!(l.crypt_name("sda"), "sda3_crypt");
        assert_eq!(l.crypt_name("nvme0n1"), "nvme0n1p3_crypt");
    }

    /// nvme0n1 and nvme1n1 recorded by id; the kernel has swapped their names.
    fn swapped(crypt_names: CryptNames) -> Layout {
        let mut c = Config::default();
        c.disks.members = vec!["nvme0n1".into(), "nvme1n1".into(), "sda".into()];
        c.disks.crypt_names = crypt_names;
        c.disks.ids.insert("nvme0n1".into(), "/by-id/A".into());
        c.disks.ids.insert("nvme1n1".into(), "/by-id/B".into());
        Layout::derive_with(&c, |id| match id {
            "/by-id/A" => Some("nvme1n1".into()),
            "/by-id/B" => Some("nvme0n1".into()),
            _ => None,
        })
    }

    #[test]
    fn devices_follow_the_id_while_mapper_names_stay_frozen() {
        let l = swapped(CryptNames::Kernel);
        assert_eq!(l.disk("nvme0n1"), "/dev/nvme1n1");
        assert_eq!(l.part("nvme0n1", 3), "/dev/nvme1n1p3");
        assert_eq!(l.crypt_name("nvme0n1"), "nvme0n1p3_crypt");
        // integrity devices are named by udev from the current kernel name.
        assert_eq!(l.int_name("nvme0n1"), "nvme1n1p3_int");
        // no id recorded: the label is the node, as before.
        assert_eq!(l.part("sda", 3), "/dev/sda3");
    }

    #[test]
    fn unresolved_id_is_addressed_by_its_own_path() {
        let mut c = Config::default();
        c.disks.members = vec!["nvme0n1".into()];
        c.disks
            .ids
            .insert("nvme0n1".into(), "/dev/disk/by-id/nvme-X_S1".into());
        let l = Layout::derive_with(&c, |_| None);
        assert_eq!(l.disk("nvme0n1"), "/dev/disk/by-id/nvme-X_S1");
        assert_eq!(l.part("nvme0n1", 2), "/dev/disk/by-id/nvme-X_S1-part2");
    }

    #[test]
    fn member_for_refuses_a_label_that_is_another_members_node() {
        let l = swapped(CryptNames::Kernel);
        assert!(l.member_for("nvme0n1").is_err());
        assert!(l.member_for("/dev/nvme1n1").is_err());
        assert_eq!(l.member_for("sda").unwrap(), "sda");
        assert!(l.member_for("sdz").is_err());
    }

    #[test]
    fn index_names_use_the_slot_and_survive_subsets_and_swaps() {
        let l = swapped(CryptNames::Index);
        assert_eq!(
            l.crypt_names(),
            ["raiden0_crypt", "raiden1_crypt", "raiden2_crypt"]
        );
        assert_eq!(l.subset(&["sda".into()]).crypt_names(), ["raiden2_crypt"]);
        let mut c = Config::default();
        c.disks.members = vec!["vda".into(), "sde".into()];
        c.disks.crypt_names = CryptNames::Index;
        let l = Layout::derive(&c).with_retired(&[("vdb".into(), "sde".into())]);
        assert_eq!(l.crypt_name("vdb"), "raiden1_crypt");
        assert_eq!(l.crypt_name("sde"), "raiden1_crypt");
    }
}
