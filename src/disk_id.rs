// stable disk identity. kernel names (nvme0n1, sda) follow probe order, which is
// async for nvme and changes between boots on real hardware, so a member is
// recorded by its udev /dev/disk/by-id link and mapped back to whatever kernel
// node it has at run time.

use crate::config::Disks;

pub const BY_ID_DIR: &str = "/dev/disk/by-id";

// by-id links that identify the disk but not as recognizably as model_serial.
const OPAQUE_ID_PREFIXES: &[&str] = &["wwn-", "nvme-eui.", "nvme-nvme.", "dm-", "md-", "lvm-"];

/// the kernel name a device path currently resolves to (the basename of its
/// canonical path, eg. a by-id link -> "nvme1n1"), or None if it does not exist.
pub fn kernel_name_of(path: &str) -> Option<String> {
    let real = std::fs::canonicalize(path).ok()?;
    real.file_name()?.to_str().map(str::to_string)
}

/// the preferred stable by-id link for a whole disk known by its kernel name, or
/// None when udev publishes none (eg. a virtio disk without a serial).
pub fn by_id_for(kernel: &str) -> Option<String> {
    let names: Vec<String> = std::fs::read_dir(BY_ID_DIR)
        .ok()?
        .flatten()
        .filter(|e| kernel_name_of(&e.path().to_string_lossy()).as_deref() == Some(kernel))
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .collect();
    preferred_id(&names).map(|n| format!("{BY_ID_DIR}/{n}"))
}

/// record the stable id of every member that has none yet, resolving its label
/// as a kernel name. only valid while the labels ARE the current kernel names
/// (at install); afterwards ids come from the luks uuids (see doctor).
pub fn record_ids(disks: &mut Disks) {
    for m in &disks.members {
        if !disks.ids.contains_key(m) {
            if let Some(id) = by_id_for(m) {
                disks.ids.insert(m.clone(), id);
            }
        }
    }
}

/// pick the most recognizable link among a disk's by-id names: a model_serial
/// form over wwn/eui ones, the shortest (no namespace "_1" suffix) on a tie, then
/// lexical order so the choice is deterministic. pure.
pub fn preferred_id(names: &[String]) -> Option<String> {
    let opaque = |n: &str| OPAQUE_ID_PREFIXES.iter().any(|p| n.starts_with(p));
    names
        .iter()
        .filter(|n| !n.contains("-part"))
        .min_by_key(|n| (opaque(n), n.len(), n.as_str()))
        .cloned()
}

/// the whole-disk kernel name holding a partition (eg. "nvme1n1p3" -> "nvme1n1"),
/// read from sysfs, where a partition's directory sits under its disk's.
pub fn parent_disk(partition: &str) -> Option<String> {
    let real = std::fs::canonicalize(format!("/sys/class/block/{partition}")).ok()?;
    if !real.join("partition").exists() {
        return None;
    }
    real.parent()?.file_name()?.to_str().map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn preferred_id_favors_model_serial_over_opaque_ids() {
        let n = names(&[
            "nvme-eui.0025385b01407a",
            "nvme-Samsung_SSD_970_EVO_Plus_1TB_S59CNJ0N802128L_1",
            "nvme-Samsung_SSD_970_EVO_Plus_1TB_S59CNJ0N802128L",
        ]);
        assert_eq!(
            preferred_id(&n).as_deref(),
            Some("nvme-Samsung_SSD_970_EVO_Plus_1TB_S59CNJ0N802128L")
        );
    }

    #[test]
    fn preferred_id_falls_back_to_opaque_and_skips_partitions() {
        assert_eq!(
            preferred_id(&names(&[
                "wwn-0x5000c500a1b2c3d4-part1",
                "wwn-0x5000c500a1b2c3d4"
            ]))
            .as_deref(),
            Some("wwn-0x5000c500a1b2c3d4")
        );
        assert_eq!(preferred_id(&[]), None);
    }
}
