// dm-crypt~md~lvm~{ext4,xfs}: per-disk dm-crypt, an md array over the crypt
// devices, lvm on top, and an ext4 or xfs root. ext4 is the recommended default.
// the two differ only in the root mkfs, the fstab line, and (for xfs) the
// xfsprogs package; everything else -- crypt, md, lvm, replace, rescue -- is
// shared, so they are one parameterized stack.

use super::Stack;
use crate::config::{Config, STACK_MD_LVM_EXT4, STACK_MD_LVM_XFS};
use crate::layout::{Layout, ROOT_MD_DEVICE, ROOT_MD_NAME};
use crate::step::Step;

#[derive(Clone, Copy)]
pub enum RootFs {
    Ext4,
    Xfs,
}

pub struct MdLvm {
    fs: RootFs,
}

impl MdLvm {
    pub fn ext4() -> Self {
        Self { fs: RootFs::Ext4 }
    }
    pub fn xfs() -> Self {
        Self { fs: RootFs::Xfs }
    }
}

impl Stack for MdLvm {
    fn id(&self) -> &str {
        match self.fs {
            RootFs::Ext4 => STACK_MD_LVM_EXT4,
            RootFs::Xfs => STACK_MD_LVM_XFS,
        }
    }

    fn packages(&self) -> Vec<String> {
        let mut p = super::pkgs(&["cryptsetup", "cryptsetup-initramfs", "mdadm", "lvm2"]);
        if matches!(self.fs, RootFs::Xfs) {
            p.push("xfsprogs".to_string());
        }
        p
    }

    fn partition_root(&self, cfg: &Config, layout: &Layout) -> Vec<Step> {
        super::crypt_partition_root(cfg, layout)
    }

    fn format_root(&self, cfg: &Config, layout: &Layout) -> Vec<Step> {
        let mut s = vec![super::md_create(
            ROOT_MD_NAME,
            &cfg.raid.level,
            &cfg.boot.bitmap,
            &layout.crypt_devices(),
        )];
        s.extend(super::lvm_create_root(ROOT_MD_DEVICE));
        s.push(match self.fs {
            RootFs::Ext4 => super::mkfs_ext4_root(),
            RootFs::Xfs => super::mkfs_xfs_root(),
        });
        s
    }

    fn mount_root(&self, _cfg: &Config, _layout: &Layout) -> Vec<Step> {
        vec![Step::run(
            "mount root filesystem at /mnt",
            &["mount", "/dev/vg0/root", "/mnt"],
        )]
    }

    fn finish(&self, _cfg: &Config, layout: &Layout) -> Vec<Step> {
        let mut s = vec![
            super::install_keyutils(),
            super::crypttab_step(layout, "luks,initramfs,keyscript=decrypt_keyctl"),
        ];
        s.extend(super::backup_luks_headers(layout));
        s.push(match self.fs {
            RootFs::Ext4 => super::fstab_root_ext4(),
            RootFs::Xfs => super::fstab_root_xfs(),
        });
        // dm-crypt aead is backed by dm_integrity, which the initrd must load.
        s.push(Step::write_mode(
            "install the dm_integrity initramfs hook",
            "/mnt/etc/initramfs-tools/hooks/integrity",
            super::INITRAMFS_HOOK_AEAD,
            0o755,
        ));
        s.push(super::update_initramfs());
        s
    }

    fn map(&self, _cfg: &Config, layout: &Layout) -> Vec<Step> {
        let mut s = super::crypt_open_disks(layout);
        s.push(super::md_assemble(ROOT_MD_NAME));
        // activate the vg explicitly: udev auto-activation is unreliable for a
        // freshly assembled (possibly degraded) array from a livecd.
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
        super::md_replace(disks, |d| layout.crypt_device(d))
    }

    fn remove(&self, _cfg: &Config, layout: &Layout, disks: &[String]) -> Vec<Step> {
        super::md_remove(
            disks,
            |d| layout.crypt_device(d),
            |d| {
                let name = layout.crypt_name(d);
                Step::run_owned(
                    format!("lock {name}"),
                    vec!["cryptsetup".to_string(), "luksClose".to_string(), name],
                )
                .best_effort()
            },
        )
    }

    fn close(&self, _cfg: &Config, layout: &Layout) -> Vec<Step> {
        let mut s = vec![
            Step::run("unmount /mnt", &["umount", "/mnt"]).best_effort(),
            super::lvm_deactivate(),
            super::md_stop(ROOT_MD_DEVICE),
        ];
        s.extend(super::crypt_close_disks(layout));
        s
    }
}
