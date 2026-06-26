// dm-crypt~bcachefs: per-disk dm-crypt with a multi-device bcachefs filesystem on
// top. bcachefs provides its own replication and checksums, so the crypt layer
// runs without aead integrity. the kernel module is out-of-tree (dkms) and the
// tools come from apt.bcachefs.org, so this stack adds that apt repo and builds
// the module against the running (livecd) kernel, like zfs. redundancy is by
// replicas (raid.level is the replica count), not parity.

use super::Stack;
use crate::config::{Config, STACK_BCACHEFS};
use crate::layout::Layout;
use crate::step::Step;

// crypttab options for this stack's per-disk members. shared between the install
// write and the running-system regen (replace --with) so the two cannot drift.
const CRYPTTAB_OPTS: &str = "luks,discard,initramfs,keyscript=decrypt_keyctl";

// the apt.bcachefs.org signing key (armored). embedded so neither the live host
// nor the target chroot needs a downloader to add the repo.
const BCACHEFS_APT_KEY: &str = r#"-----BEGIN PGP PUBLIC KEY BLOCK-----

mDMEaMmnZRYJKwYBBAHaRw8BAQdAcK+CJMNI+3Sndv0eiS1US4Z8fBniaqxXCZ/p
52ME2MO0VERlYmlhbiBBdXRvbWF0ZWQgUGFja2FnZXIgKGFwdC5iY2FjaGVmcy5v
cmcgQ0kgYm90KSA8bGludXgtYmNhY2hlZnNAdmdlci5rZXJuZWwub3JnPoiQBBMW
CgA4FiEEZNpXvRnN40DTstpI91eP+7KEqmEFAmjJp2UCGwEFCwkIBwIGFQoJCAsC
BBYCAwECHgECF4AACgkQ91eP+7KEqmG7dwEAkhxfP2Wx34qMEDJsQBWyYPlJLfYS
XnA2WRWU8AJGyCYBANEnjIubG5IgdQn/iSiFwRpVpZvDWiosQfus+TGzjlgDuDME
aMmn8hYJKwYBBAHaRw8BAQdAIuefCkHxOLACQB/DFzX4ziweykmAXhiiGHb4qGyp
/ACI7wQYFgoAIBYhBGTaV70ZzeNA07LaSPdXj/uyhKphBQJoyafyAhsCAIEJEPdX
j/uyhKphdiAEGRYKAB0WIQTqSDuZECDHKopQNa2gYgteDgHB3QUCaMmn8gAKCRCg
YgteDgHB3ZCOAP9pBHXdV7ufM0j8jMwrGh1UfiKjHYKNkdMG7W0N6p1PBgEA9k4F
OPgr+eQ12WPbPhmOrrJLg51fED0dOU/CrmVdSwR07gEAzGWg5sulro5jKxnVr3Ut
FqAq/x1a4QERZ1bUrTbF+IcBAKxSjotqdVEb0K0gEvAiqotK4xkuZF5QwQ2skF4K
s0MPuDMEaMmoMRYJKwYBBAHaRw8BAQdANqYoypkyAHLXjJojueqrAsXFrplPIAGp
wdZJ415y7LWIeAQYFgoAIBYhBGTaV70ZzeNA07LaSPdXj/uyhKphBQJoyagxAhsg
AAoJEPdXj/uyhKphWzMBAOhv1SHXg9cQ5m1q/k34bM62sAplGyjm1qb906Gj7o4y
AQDWxFEbHSn0nKBngo0oewa017QLvOLzhad9SlbVtbY4Ag==
=C90X
-----END PGP PUBLIC KEY BLOCK-----
"#;

const KEYRING: &str = "/etc/apt/keyrings/apt.bcachefs.org.asc";

pub struct BcachefsStack;

impl Stack for BcachefsStack {
    fn id(&self) -> &str {
        STACK_BCACHEFS
    }

    fn packages(&self) -> Vec<String> {
        super::pkgs(&[
            "cryptsetup",
            "cryptsetup-initramfs",
            "bcachefs-tools",
            "bcachefs-kernel-dkms",
        ])
    }

    // bcachefs-tools + the dkms module live in apt.bcachefs.org, not Debian. add
    // the repo (key + deb822 source) on the live host (root="") and in the target
    // chroot (root="/mnt"); the suite is per-release (forky/, trixie/, ...).
    fn apt_repos(&self, cfg: &Config, root: &str) -> Vec<Step> {
        let sources = format!(
            "Types: deb\nURIs: https://apt.bcachefs.org/{}/\nSuites: bcachefs-tools-release\nComponents: main\nSigned-By: {KEYRING}\n",
            cfg.install.release
        );
        let mut s = vec![
            Step::run_owned(
                format!("create {root}/etc/apt/keyrings"),
                vec![
                    "mkdir".into(),
                    "-p".into(),
                    format!("{root}/etc/apt/keyrings"),
                ],
            ),
            Step::write(
                "install the bcachefs apt signing key",
                format!("{root}{KEYRING}"),
                BCACHEFS_APT_KEY,
            ),
            Step::write(
                "add the bcachefs apt source",
                format!("{root}/etc/apt/sources.list.d/apt.bcachefs.org.sources"),
                sources,
            ),
            // pin the repo below Debian's default (500): only the out-of-tree
            // dkms module is taken from it. bcachefs-tools must come from the
            // matching debian release -- the repo's per-suite tools can lag the
            // distro's libs (eg. a stale libsodium dep) and fail to install.
            Step::write(
                "pin the bcachefs repo below debian",
                format!("{root}/etc/apt/preferences.d/apt.bcachefs.org.pref"),
                "Package: *\nPin: release o=apt.bcachefs.org\nPin-Priority: 100\n",
            ),
        ];
        let update = Step::run(
            "refresh package lists (bcachefs repo)",
            &["apt-get", "update"],
        );
        s.push(if root.is_empty() {
            update
        } else {
            update.chroot()
        });
        s
    }

    // the dkms module builds against installed headers; on the livecd those for
    // the running kernel may be absent, so install them (as zfs does).
    fn host_prereqs(&self) -> Vec<Step> {
        vec![Step::sh(
            "install running-kernel headers on host (bcachefs-dkms)",
            "env DEBIAN_FRONTEND=noninteractive apt-get install -y \"linux-headers-$(uname -r)\"",
        )]
    }

    fn partition_root(&self, cfg: &Config, layout: &Layout) -> Vec<Step> {
        super::crypt_partition_root(cfg, layout)
    }

    fn format_root(&self, cfg: &Config, layout: &Layout) -> Vec<Step> {
        let mut a = vec![
            "mkfs.bcachefs".to_string(),
            "-f".to_string(),
            format!("--replicas={}", cfg.raid.level),
        ];
        a.extend(layout.crypt_devices());
        vec![
            super::modprobe("bcachefs"),
            Step::run_owned(format!("create bcachefs (replicas {})", cfg.raid.level), a),
        ]
    }

    fn mount_root(&self, _cfg: &Config, layout: &Layout) -> Vec<Step> {
        // multi-device bcachefs mounts from a colon-joined device list.
        let devs = layout.crypt_devices().join(":");
        vec![Step::run_owned(
            "mount bcachefs root at /mnt",
            vec![
                "mount".to_string(),
                "-t".to_string(),
                "bcachefs".to_string(),
                devs,
                "/mnt".to_string(),
            ],
        )]
    }

    fn finish(&self, _cfg: &Config, layout: &Layout) -> Vec<Step> {
        let mut s = vec![
            super::install_keyutils(),
            // initramfs: pull every crypt member into the initrd so the
            // multi-device bcachefs root can unlock all its devices at boot.
            super::crypttab_step(layout, CRYPTTAB_OPTS, "/mnt/etc/crypttab"),
        ];
        s.extend(super::backup_luks_headers(layout, "/mnt/boot"));
        s.push(super::fstab_root_bcachefs(layout));
        s.push(super::update_initramfs());
        s
    }

    fn map(&self, _cfg: &Config, layout: &Layout) -> Vec<Step> {
        let mut s = vec![super::modprobe("bcachefs")];
        s.extend(super::crypt_open_disks(layout));
        s
    }

    fn status(&self, _cfg: &Config, _layout: &Layout) -> Vec<Step> {
        vec![
            Step::run("bcachefs fs usage", &["bcachefs", "fs", "usage", "/"]).best_effort(),
            Step::sh(
                "bcachefs device state",
                "bcachefs show-super /dev/mapper/*_crypt 2>/dev/null | grep -iE 'state|durability|label' || true",
            ),
        ]
    }

    fn scrub(&self, _cfg: &Config, _layout: &Layout) -> Vec<Step> {
        vec![Step::run(
            "scrub the bcachefs data",
            &["bcachefs", "data", "scrub", "/"],
        )
        .best_effort()]
    }

    fn replace(&self, _cfg: &Config, layout: &Layout, disks: &[String]) -> Vec<Step> {
        let mut s = Vec::new();
        for d in disks {
            let dev = layout.crypt_device(d);
            s.push(Step::run_owned(
                format!("add {dev} to the bcachefs"),
                vec![
                    "bcachefs".to_string(),
                    "device".to_string(),
                    "add".to_string(),
                    "--force".to_string(),
                    "/".to_string(),
                    dev,
                ],
            ));
        }
        // drop the faulty members the new devices replaced.
        s.push(Step::run(
            "remove the missing bcachefs device(s)",
            &["bcachefs", "device", "remove", "--force", "missing", "/"],
        ));
        s.push(Step::run_owned(
            "re-replicate onto the new devices",
            vec![
                "bcachefs".to_string(),
                "data".to_string(),
                "rereplicate".to_string(),
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

    fn crypttab_regen(&self, layout: &Layout) -> Option<Step> {
        Some(super::crypttab_step(layout, CRYPTTAB_OPTS, "/etc/crypttab"))
    }

    fn initramfs_binaries(&self) -> Vec<&'static str> {
        let mut b = super::crypt_initramfs_binaries();
        b.push("bcachefs");
        b
    }

    // bcachefs, like btrfs, refuses a multi-device mount with a faulty member and
    // needs a manual degraded mount from a surviving crypt device.
    fn recover_actions(
        &self,
        _cfg: &Config,
        layout: &Layout,
        at: &str,
    ) -> Vec<super::RecoverAction> {
        let devs = layout.crypt_devices().join(" ");
        vec![
            super::RecoverAction::new(
                "scan bcachefs devices",
                vec![Step::sh("scan bcachefs devices", "bcachefs device scan 2>/dev/null || true")
                    .best_effort()],
            ),
            super::RecoverAction::new(
                format!("mount the bcachefs root (degraded) at {at}"),
                vec![Step::sh(
                    format!("mount the bcachefs root (degraded) at {at}"),
                    format!(
                        "for d in {devs}; do mount -t bcachefs -o degraded \"$d\" {at} 2>/dev/null && break; done"
                    ),
                )],
            ),
        ]
    }

    fn close(&self, _cfg: &Config, layout: &Layout) -> Vec<Step> {
        let mut s = vec![Step::run("unmount /mnt", &["umount", "/mnt"]).best_effort()];
        s.extend(super::crypt_close_disks(layout));
        s
    }
}
