// dm-crypt~zfs: per-disk dm-crypt with a zfs pool (rpool) on top. zfs provides
// its own raid and checksums. the pool is created with an altroot so its root
// dataset mounts at /mnt during install.

use super::Stack;
use crate::config::{Config, STACK_ZFS};
use crate::layout::{Layout, ZPOOL_NAME};
use crate::step::Step;

const ROOT_DATASET: &str = "rpool/ROOT/debian";

// crypttab options for this stack's per-disk members. shared between the install
// write and the running-system regen (replace --with) so the two cannot drift.
const CRYPTTAB_OPTS: &str = "luks,discard,initramfs,keyscript=decrypt_keyctl";

pub struct ZfsStack;

impl Stack for ZfsStack {
    fn id(&self) -> &str {
        STACK_ZFS
    }

    fn packages(&self) -> Vec<String> {
        super::pkgs(&[
            "cryptsetup",
            "cryptsetup-initramfs",
            "linux-headers-amd64",
            "zfs-dkms",
            "zfsutils-linux",
        ])
    }

    fn host_prereqs(&self) -> Vec<Step> {
        // zfs-dkms builds against installed kernel headers; on the livecd those
        // must match the RUNNING kernel. linux-headers-amd64 (right for the
        // target) can pull a newer kernel whose module the running kernel cannot
        // modprobe, so install the running kernel's headers explicitly here.
        vec![Step::sh(
            "install running-kernel headers on host (zfs-dkms)",
            "env DEBIAN_FRONTEND=noninteractive apt-get install -y \"linux-headers-$(uname -r)\"",
        )]
    }

    fn host_packages(&self) -> Vec<String> {
        // drop linux-headers-amd64 (the target kernel's headers) from the host
        // set; host_prereqs installs the running kernel's headers instead.
        super::pkgs(&[
            "cryptsetup",
            "cryptsetup-initramfs",
            "zfs-dkms",
            "zfsutils-linux",
        ])
    }

    fn partition_root(&self, cfg: &Config, layout: &Layout) -> Vec<Step> {
        super::crypt_partition_root(cfg, layout)
    }

    fn format_root(&self, cfg: &Config, layout: &Layout) -> Vec<Step> {
        let mut create = vec![
            "zpool",
            "create",
            "-f",
            "-o",
            "ashift=12",
            "-O",
            "acltype=posixacl",
            "-O",
            "canmount=off",
            "-O",
            "compression=lz4",
            "-O",
            "dnodesize=auto",
            "-O",
            "normalization=formD",
            "-O",
            "relatime=on",
            "-O",
            "xattr=sa",
            "-O",
            "mountpoint=/",
            "-R",
            "/mnt",
            ZPOOL_NAME,
        ]
        .iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>();
        create.push(cfg.raid.level.clone());
        create.extend(layout.crypt_devices());
        vec![
            Step::run("load the zfs module", &["modprobe", "zfs"]),
            Step::run_owned(
                format!("create zpool {ZPOOL_NAME} ({})", cfg.raid.level),
                create,
            ),
            Step::run(
                "create the ROOT container dataset",
                &[
                    "zfs",
                    "create",
                    "-o",
                    "canmount=off",
                    "-o",
                    "mountpoint=none",
                    "rpool/ROOT",
                ],
            ),
            Step::run(
                "create the debian root dataset",
                &[
                    "zfs",
                    "create",
                    "-o",
                    "canmount=noauto",
                    "-o",
                    "mountpoint=/",
                    ROOT_DATASET,
                ],
            ),
            Step::run("mount the root dataset", &["zfs", "mount", ROOT_DATASET]),
        ]
    }

    fn mount_root(&self, _cfg: &Config, _layout: &Layout) -> Vec<Step> {
        vec![Step::run(
            "wait for the root dataset",
            &["zfs", "wait", ROOT_DATASET],
        )]
    }

    fn finish(&self, _cfg: &Config, layout: &Layout) -> Vec<Step> {
        let mut s = vec![
            Step::run(
                "install zfs initramfs support and keyutils in target",
                &["apt-get", "install", "-y", "zfs-initramfs", "keyutils"],
            )
            .chroot(),
            super::crypttab_step(layout, CRYPTTAB_OPTS, "/mnt/etc/crypttab"),
        ];
        s.extend(super::backup_luks_headers(layout, "/mnt/boot"));
        s.push(Step::write(
            "force the zfs initrd to be rebuilt by dkms",
            "/mnt/etc/dkms/zfs.conf",
            "REMAKE_INITRD=yes\n",
        ));
        s.push(Step::write(
            "point grub at the zfs root dataset",
            "/mnt/etc/default/grub.d/zfs.cfg",
            "GRUB_CMDLINE_LINUX=\"root=ZFS=rpool/ROOT/debian\"\n",
        ));
        s.push(Step::run("regenerate grub config", &["update-grub"]).chroot());
        s.push(super::update_initramfs());
        s
    }

    fn map(&self, _cfg: &Config, layout: &Layout) -> Vec<Step> {
        let mut s = super::crypt_open_disks(layout);
        s.push(Step::run("load the zfs module", &["modprobe", "zfs"]));
        s.push(Step::run(
            "import the pool under /mnt",
            &["zpool", "import", "-f", "-N", "-R", "/mnt", ZPOOL_NAME],
        ));
        s.push(Step::run(
            "mount the root dataset",
            &["zfs", "mount", ROOT_DATASET],
        ));
        s
    }

    fn status(&self, _cfg: &Config, _layout: &Layout) -> Vec<Step> {
        vec![Step::run("zpool status", &["zpool", "status", "-v", ZPOOL_NAME]).best_effort()]
    }

    fn scrub(&self, _cfg: &Config, _layout: &Layout) -> Vec<Step> {
        vec![
            Step::run("scrub the pool", &["zpool", "scrub", "-w", ZPOOL_NAME]),
            Step::run("clear pool errors", &["zpool", "clear", ZPOOL_NAME]),
        ]
    }

    fn replace(&self, _cfg: &Config, layout: &Layout, disks: &[String]) -> Vec<Step> {
        // remove offlined the old vdev and partition recreated a fresh luks
        // device at the same path; the single-arg replace resilvers onto it.
        let mut s: Vec<Step> = disks
            .iter()
            .map(|d| {
                let dev = layout.crypt_device(d);
                Step::run_owned(
                    format!("replace the vdev at {dev}"),
                    vec![
                        "zpool".to_string(),
                        "replace".to_string(),
                        "-f".to_string(),
                        ZPOOL_NAME.to_string(),
                        dev,
                    ],
                )
            })
            .collect();
        s.push(Step::run(
            "wait for the resilver",
            &["zpool", "wait", ZPOOL_NAME],
        ));
        s
    }

    fn remove(&self, _cfg: &Config, layout: &Layout, disks: &[String]) -> Vec<Step> {
        let mut s = Vec::new();
        for d in disks {
            let dev = layout.crypt_device(d);
            s.push(
                Step::run_owned(
                    format!("offline the vdev at {dev}"),
                    vec![
                        "zpool".to_string(),
                        "offline".to_string(),
                        ZPOOL_NAME.to_string(),
                        dev,
                    ],
                )
                .best_effort(),
            );
            let name = layout.crypt_name(d);
            s.push(
                Step::run_owned(
                    format!("lock {name}"),
                    vec!["cryptsetup".to_string(), "luksClose".to_string(), name],
                )
                .best_effort(),
            );
        }
        s
    }

    fn crypttab_regen(&self, layout: &Layout) -> Option<Step> {
        Some(super::crypttab_step(layout, CRYPTTAB_OPTS, "/etc/crypttab"))
    }

    fn initramfs_binaries(&self) -> Vec<&'static str> {
        let mut b = super::crypt_initramfs_binaries();
        b.extend(["zpool", "zfs"]);
        b
    }

    // zfs auto-imports and mounts degraded at boot, so a drop to the rescue shell
    // is rare; recover force-imports the pool (under the altroot `at`) and mounts
    // the root dataset, for the case it stalled.
    fn recover_actions(
        &self,
        _cfg: &Config,
        _layout: &Layout,
        at: &str,
    ) -> Vec<super::RecoverAction> {
        vec![
            super::RecoverAction::new(
                "import the zfs pool (forced)",
                vec![
                    Step::run("load the zfs module", &["modprobe", "zfs"]).best_effort(),
                    Step::run_owned(
                        format!("import {ZPOOL_NAME} under {at}"),
                        vec![
                            "zpool".into(),
                            "import".into(),
                            "-f".into(),
                            "-N".into(),
                            "-R".into(),
                            at.into(),
                            ZPOOL_NAME.into(),
                        ],
                    )
                    .best_effort(),
                ],
            ),
            super::RecoverAction::new(
                format!("mount the root dataset at {at}"),
                vec![Step::run_owned(
                    format!("mount {ROOT_DATASET}"),
                    vec!["zfs".into(), "mount".into(), ROOT_DATASET.into()],
                )],
            ),
        ]
    }

    fn close(&self, _cfg: &Config, layout: &Layout) -> Vec<Step> {
        let mut s = vec![
            Step::run("unmount /mnt", &["umount", "/mnt"]).best_effort(),
            Step::run("export all pools", &["zpool", "export", "-a"]).best_effort(),
            Step::run("destroy the pool", &["zpool", "destroy", ZPOOL_NAME]).best_effort(),
        ];
        s.extend(super::crypt_close_disks(layout));
        s
    }

    fn backports_pins(&self, release: &str) -> Option<String> {
        Some(format!(
            "Package: libnvpair1linux libuutil1linux libzfs2linux libzfslinux-dev libzpool2linux python3-pyzfs pyzfs-doc spl spl-dkms zfs-dkms zfs-dracut zfs-initramfs zfs-test zfsutils-linux zfsutils-linux-dev zfs-zed\nPin: release n={release}-backports\nPin-Priority: 990\n"
        ))
    }
}
