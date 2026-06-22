// dm-crypt~btrfs: per-disk dm-crypt with a multi-device btrfs filesystem on top.
// btrfs provides its own raid and checksums, so the crypt layer usually runs
// without aead integrity.

use super::Stack;
use crate::config::{Config, STACK_BTRFS};
use crate::layout::Layout;
use crate::step::Step;

pub struct BtrfsStack;

impl Stack for BtrfsStack {
    fn id(&self) -> &str {
        STACK_BTRFS
    }

    fn packages(&self) -> Vec<String> {
        super::pkgs(&["cryptsetup", "cryptsetup-initramfs", "btrfs-progs"])
    }

    fn partition_root(&self, cfg: &Config, layout: &Layout) -> Vec<Step> {
        super::crypt_partition_root(cfg, layout)
    }

    fn format_root(&self, cfg: &Config, layout: &Layout) -> Vec<Step> {
        let mut a = vec![
            "mkfs.btrfs".to_string(),
            "-f".to_string(),
            "--csum".to_string(),
            cfg.btrfs.csum.clone(),
            "-m".to_string(),
            cfg.metadata_level().to_string(),
            "-d".to_string(),
            cfg.raid.level.clone(),
        ];
        a.extend(layout.crypt_devices());
        vec![Step::run_owned(
            format!(
                "create btrfs (data {}, metadata {}, csum {})",
                cfg.raid.level,
                cfg.metadata_level(),
                cfg.btrfs.csum
            ),
            a,
        )]
    }

    fn mount_root(&self, _cfg: &Config, layout: &Layout) -> Vec<Step> {
        let dev = layout
            .crypt_devices()
            .into_iter()
            .next()
            .unwrap_or_default();
        vec![
            Step::run_owned(
                "mount btrfs root at /mnt",
                vec!["mount".to_string(), dev, "/mnt".to_string()],
            ),
            Step::run(
                "balance the new array",
                &["btrfs", "balance", "start", "--full-balance", "/mnt"],
            ),
            Step::run(
                "scrub the new array",
                &["btrfs", "scrub", "start", "-B", "/mnt"],
            ),
        ]
    }

    fn finish(&self, _cfg: &Config, layout: &Layout) -> Vec<Step> {
        let mut s = vec![
            super::install_keyutils(),
            // initramfs: pull every crypt member into the initrd so the
            // multi-device btrfs root can unlock all its devices at boot (matching
            // the md and zfs stacks); without it the boot drops to the initramfs.
            super::crypttab_step(layout, "luks,discard,initramfs,keyscript=decrypt_keyctl"),
        ];
        s.extend(super::backup_luks_headers(layout));
        s.push(super::fstab_root_btrfs(layout));
        s.push(super::update_initramfs());
        s
    }

    fn map(&self, _cfg: &Config, layout: &Layout) -> Vec<Step> {
        super::crypt_open_disks(layout)
    }

    fn status(&self, _cfg: &Config, _layout: &Layout) -> Vec<Step> {
        vec![
            Step::run("btrfs filesystem", &["btrfs", "filesystem", "show", "/"]).best_effort(),
            Step::run("btrfs device stats", &["btrfs", "device", "stats", "/"]).best_effort(),
            Step::run(
                "btrfs scrub status",
                &["btrfs", "scrub", "status", "-d", "/"],
            )
            .best_effort(),
            Step::sh(
                "checksum errors from the kernel ring buffer",
                r#"dmesg -c | grep "checksum error at" | grep "(path:" | sed -n -r 's#.*BTRFS.*i/o error.*path: (.*)\)#\1#p' || true"#,
            ),
        ]
    }

    fn scrub(&self, _cfg: &Config, layout: &Layout) -> Vec<Step> {
        let mut s = Vec::new();
        for dev in layout.crypt_devices() {
            s.push(
                Step::run_owned(
                    format!("scrub {dev}"),
                    vec![
                        "btrfs".to_string(),
                        "scrub".to_string(),
                        "start".to_string(),
                        "-B".to_string(),
                        dev,
                    ],
                )
                .best_effort(),
            );
        }
        for dev in layout.crypt_devices() {
            s.push(Step::run_owned(
                format!("reset stats for {dev}"),
                vec![
                    "btrfs".to_string(),
                    "device".to_string(),
                    "stats".to_string(),
                    "-z".to_string(),
                    dev,
                ],
            ));
        }
        s
    }

    fn replace(&self, cfg: &Config, layout: &Layout, disks: &[String]) -> Vec<Step> {
        let mut s = Vec::new();
        for d in disks {
            let dev = layout.crypt_device(d);
            s.push(Step::run_owned(
                format!("add {dev} to the volume"),
                vec![
                    "btrfs".to_string(),
                    "device".to_string(),
                    "add".to_string(),
                    dev,
                    "/".to_string(),
                ],
            ));
            s.push(Step::run(
                "remove the missing device",
                &["btrfs", "device", "remove", "missing", "/"],
            ));
        }
        s.push(Step::run_owned(
            "rebalance onto the new raid profile",
            vec![
                "btrfs".to_string(),
                "balance".to_string(),
                "start".to_string(),
                format!("-dconvert={},soft", cfg.raid.level),
                format!("-mconvert={},soft", cfg.metadata_level()),
                "/".to_string(),
            ],
        ));
        s
    }

    fn remove(&self, _cfg: &Config, layout: &Layout, disks: &[String]) -> Vec<Step> {
        disks
            .iter()
            .map(|d| {
                let name = layout.crypt_name(d);
                Step::run_owned(
                    format!("lock {name}"),
                    vec!["cryptsetup".to_string(), "luksClose".to_string(), name],
                )
                .best_effort()
            })
            .collect()
    }

    fn close(&self, _cfg: &Config, layout: &Layout) -> Vec<Step> {
        let mut s = vec![Step::run("unmount /mnt", &["umount", "/mnt"]).best_effort()];
        s.extend(super::crypt_close_disks(layout));
        s
    }
}
