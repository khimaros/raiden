// dm-integrity~md~dm-crypt~lvm~ext4: dm-integrity below md, a single dm-crypt
// over the whole md array, then lvm and ext4. integrity sits below md here, so
// these helpers are local to this stack rather than shared.

use super::Stack;
use crate::config::{Config, STACK_MD_INTEGRITY};
use crate::layout::{Layout, ROOT_MD_DEVICE, ROOT_MD_NAME};
use crate::step::Step;

const ROOT_CRYPT_NAME: &str = "md_root_crypt";
const ROOT_CRYPT_DEVICE: &str = "/dev/mapper/md_root_crypt";

pub struct MdIntegrity;

impl Stack for MdIntegrity {
    fn id(&self) -> &str {
        STACK_MD_INTEGRITY
    }

    fn packages(&self) -> Vec<String> {
        super::pkgs(&["cryptsetup", "cryptsetup-initramfs", "mdadm", "lvm2"])
    }

    fn partition_root(&self, cfg: &Config, layout: &Layout) -> Vec<Step> {
        let mut s = super::create_root_partitions(cfg, layout);
        s.extend(integrity_format_disks(cfg, layout));
        s.extend(integrity_open_disks(cfg, layout));
        s
    }

    fn format_root(&self, cfg: &Config, layout: &Layout) -> Vec<Step> {
        let mut s = vec![super::md_create(
            ROOT_MD_NAME,
            &cfg.raid.level,
            &cfg.boot.bitmap,
            &layout.int_devices(),
        )];
        s.push(super::crypt_format_device(
            cfg,
            ROOT_MD_DEVICE,
            format!("luks-format {ROOT_MD_DEVICE}"),
        ));
        s.push(super::crypt_open_device(
            ROOT_MD_DEVICE,
            ROOT_CRYPT_NAME,
            format!("unlock {ROOT_MD_DEVICE} as {ROOT_CRYPT_NAME}"),
        ));
        s.extend(super::lvm_create_root(ROOT_CRYPT_DEVICE));
        s.push(super::mkfs_ext4_root());
        s
    }

    fn mount_root(&self, _cfg: &Config, _layout: &Layout) -> Vec<Step> {
        vec![Step::run(
            "mount root filesystem at /mnt",
            &["mount", "/dev/vg0/root", "/mnt"],
        )]
    }

    fn finish(&self, cfg: &Config, _layout: &Layout) -> Vec<Step> {
        vec![
            Step::sh(
                "write the md crypt entry to crypttab",
                format!(
                    "uuid=$(blkid -s UUID -o value {ROOT_MD_DEVICE}); echo \"{ROOT_CRYPT_NAME} UUID=$uuid none luks,discard\" >> /mnt/etc/crypttab"
                ),
            ),
            super::fstab_root_ext4(),
            Step::write(
                "install the dm-integrity udev rules",
                "/mnt/etc/udev/rules.d/99-integrity.rules",
                udev_rules(&cfg.integrity.algorithm),
            ),
            Step::write_mode(
                "install the dm-integrity initramfs hook",
                "/mnt/etc/initramfs-tools/hooks/integrity",
                super::INITRAMFS_HOOK_INTEGRITY,
                0o755,
            ),
            super::update_initramfs(),
        ]
    }

    fn map(&self, cfg: &Config, layout: &Layout) -> Vec<Step> {
        let mut s = integrity_open_disks(cfg, layout);
        s.push(super::md_assemble(ROOT_MD_NAME));
        s.push(super::crypt_open_device(
            ROOT_MD_DEVICE,
            ROOT_CRYPT_NAME,
            format!("unlock {ROOT_MD_DEVICE} as {ROOT_CRYPT_NAME}"),
        ));
        s.push(super::lvm_activate());
        s
    }

    fn status(&self, _cfg: &Config, _layout: &Layout) -> Vec<Step> {
        super::md_status()
    }

    fn scrub(&self, _cfg: &Config, _layout: &Layout) -> Vec<Step> {
        super::md_scrub()
    }

    fn replace(&self, _cfg: &Config, layout: &Layout, disks: &[String]) -> Vec<Step> {
        super::md_replace(disks, |d| layout.int_device_of(d))
    }

    fn remove(&self, _cfg: &Config, layout: &Layout, disks: &[String]) -> Vec<Step> {
        super::md_remove(
            disks,
            |d| layout.int_device_of(d),
            |d| {
                let name = layout.int_name(d);
                Step::run_owned(
                    format!("close integrity device {name}"),
                    vec!["integritysetup".to_string(), "close".to_string(), name],
                )
                .best_effort()
            },
        )
    }

    fn close(&self, _cfg: &Config, layout: &Layout) -> Vec<Step> {
        let mut s = vec![
            Step::run("unmount /mnt", &["umount", "/mnt"]).best_effort(),
            super::lvm_deactivate(),
            Step::run_owned(
                format!("lock {ROOT_CRYPT_NAME}"),
                vec![
                    "cryptsetup".to_string(),
                    "luksClose".to_string(),
                    ROOT_CRYPT_NAME.to_string(),
                ],
            )
            .best_effort(),
            super::md_stop(ROOT_MD_DEVICE),
        ];
        s.extend(integrity_close_disks(layout));
        s
    }

    fn backports_pins(&self, release: &str) -> Option<String> {
        Some(format!(
            "Package: linux-image linux-image-amd64 linux-headers firmware-linux firmware-linux-nonfree\nPin: release n={release}-backports\nPin-Priority: 990\n"
        ))
    }
}

fn integrity_format_disks(cfg: &Config, layout: &Layout) -> Vec<Step> {
    layout
        .root_devices()
        .iter()
        .map(|dev| {
            Step::run_owned(
                format!("integrity-format {dev}"),
                vec![
                    "integritysetup".to_string(),
                    "format".to_string(),
                    "--batch-mode".to_string(),
                    "--sector-size=4096".to_string(),
                    format!("--integrity={}", cfg.integrity.algorithm),
                    dev.clone(),
                ],
            )
        })
        .collect()
}

fn integrity_open_disks(cfg: &Config, layout: &Layout) -> Vec<Step> {
    layout
        .members
        .iter()
        .map(|d| {
            let dev = layout.part(d, 3);
            let name = layout.int_name(d);
            Step::run_owned(
                format!("integrity-open {dev} as {name}"),
                vec![
                    "integritysetup".to_string(),
                    "open".to_string(),
                    format!("--integrity={}", cfg.integrity.algorithm),
                    dev,
                    name,
                ],
            )
        })
        .collect()
}

fn integrity_close_disks(layout: &Layout) -> Vec<Step> {
    layout
        .int_names()
        .into_iter()
        .map(|name| {
            Step::run_owned(
                format!("close integrity device {name}"),
                vec!["integritysetup".to_string(), "close".to_string(), name],
            )
            .best_effort()
        })
        .collect()
}

/// udev rules that open/close each member's dm-integrity device on hotplug, so
/// the array can assemble at boot from the initrd.
fn udev_rules(algo: &str) -> String {
    format!(
        "ACTION==\"add\", SUBSYSTEM==\"block\", ENV{{DEVTYPE}}==\"partition\", ENV{{ID_FS_TYPE}}==\"DM_integrity\", RUN+=\"/sbin/integritysetup open --integrity={algo} $env{{DEVNAME}} %k_int\"\n\
         ACTION==\"remove\", SUBSYSTEM==\"block\", ENV{{DEVTYPE}}==\"partition\", ENV{{ID_FS_TYPE}}==\"DM_integrity\", RUN+=\"/sbin/integritysetup close %k_int\"\n"
    )
}
