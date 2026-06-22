// assemble plans as ordered phases, mirroring the predecessor's phase scripts.
// each phase is a reusable builder so operations (install, rescue, replace) can
// compose the subset they need. file edits are emitted either as native writes
// (static content) or as `sh -c` (when a runtime uuid from blkid is needed).

use crate::config::Config;
use crate::layout::{Layout, BOOT_MD_DEVICE, BOOT_MD_NAME, ESP_LINK};
use crate::stack::{self, Stack};
use crate::state::State;
use crate::step::{Phase, Step};

pub const APT: &str = "apt";
pub const PREPARE: &str = "prepare";
pub const PARTITION: &str = "partition";
pub const FORMAT: &str = "format";
pub const MOUNT: &str = "mount";
pub const BIND: &str = "bind";
pub const STRAP: &str = "strap";
pub const INSTALL: &str = "install";
pub const BOOTLOADER: &str = "bootloader";
pub const FINISH: &str = "finish";
pub const CLOSE: &str = "close";

pub const PHASES: &[&str] = &[
    APT, PREPARE, PARTITION, FORMAT, MOUNT, STRAP, BIND, INSTALL, BOOTLOADER, FINISH, CLOSE,
];

// vfat mount options for every esp, matching what grub-install would write.
const EFI_OPTS: &str =
    "rw,relatime,fmask=0022,dmask=0022,codepage=437,iocharset=ascii,shortname=mixed,utf8,errors=remount-ro";

/// the full install pipeline for the configured stack. when `auto_root_password`
/// is set (an unattended run with a password source), the root password is set
/// non-interactively to the disk password; otherwise raiden prompts for it.
pub fn install(
    cfg: &Config,
    layout: &Layout,
    stack: &dyn Stack,
    auto_root_password: bool,
) -> Vec<Phase> {
    vec![
        apt_phase(cfg, stack),
        prepare_phase(),
        partition_phase(cfg, layout, stack),
        format_phase(cfg, layout, stack),
        Phase::new(MOUNT, stack.mount_root(cfg, layout)),
        // strap must precede bind: debootstrap creates /mnt/{dev,proc,sys,run}.
        strap_phase(cfg),
        bind_phase(cfg, layout),
        install_base_phase(cfg, stack, auto_root_password),
        bootloader_phase(cfg, layout),
        finish_phase(cfg, layout, stack),
        close_phase(cfg, layout, stack),
    ]
}

/// the stack's finish steps plus the install manifest, written into the target
/// while /mnt is still mounted so it persists into the installed system. ops on
/// the running system resolve their config from this manifest.
fn finish_phase(cfg: &Config, layout: &Layout, stack: &dyn Stack) -> Phase {
    let mut s = stack.finish(cfg, layout);
    let manifest = State::from_config(cfg).to_toml();
    s.push(Step::run(
        "create the raiden state dirs in the target",
        &["mkdir", "-p", "/mnt/etc/raiden", "/mnt/boot/raiden"],
    ));
    s.push(Step::write(
        "write the install manifest",
        "/mnt/etc/raiden/state.toml",
        manifest.clone(),
    ));
    s.push(Step::write(
        "mirror the manifest to /boot",
        "/mnt/boot/raiden/state.toml",
        manifest,
    ));
    // independent /boot: with the final initrd and the manifest now in /boot, push
    // them to every mirror so a survivor boots an identical, working /boot.
    if !layout.boot_raid() {
        s.push(boot_mirror_sync_step());
    }
    Phase::new(FINISH, s)
}

fn apt_get(args: &[&str]) -> Vec<String> {
    let mut a = vec![
        "env".to_string(),
        "DEBIAN_FRONTEND=noninteractive".to_string(),
        "apt-get".to_string(),
    ];
    a.extend(args.iter().map(|s| s.to_string()));
    a
}

fn apt_install(note: impl Into<String>, pkgs: &[String]) -> Step {
    let mut a = apt_get(&["install", "-y"]);
    a.extend(pkgs.iter().cloned());
    Step::run_owned(note, a)
}

fn strs(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

pub fn apt_phase(cfg: &Config, stack: &dyn Stack) -> Phase {
    let mut s = vec![
        // contrib is needed on the host for zfs; harmless otherwise.
        Step::sh(
            "enable contrib on the host",
            "sed -i 's/ main/ main contrib/g' /etc/apt/sources.list",
        ),
    ];
    if cfg.install.backports {
        s.push(Step::write(
            "enable backports on the host",
            "/etc/apt/sources.list.d/backports.list",
            format!(
                "deb http://deb.debian.org/debian/ {}-backports main contrib\n",
                cfg.install.release
            ),
        ));
    }
    s.push(Step::run_owned(
        "refresh package lists",
        apt_get(&["update"]),
    ));
    s.push(apt_install(
        "install host provisioning tools",
        &strs(&["gdisk", "dosfstools", "mdadm", "debootstrap", "rsync"]),
    ));
    if cfg.install.boot_mode == "efi" {
        s.push(apt_install("install efi tools", &strs(&["efibootmgr"])));
    }
    s.extend(stack.apt_repos(cfg, ""));
    s.extend(stack.host_prereqs());
    s.push(apt_install(
        "install stack packages on host",
        &stack.host_packages(),
    ));
    if !cfg.install.extra_packages.is_empty() {
        s.push(apt_install(
            "install extra packages on host",
            &cfg.install.extra_packages,
        ));
    }
    Phase::new(APT, s)
}

pub fn prepare_phase() -> Phase {
    Phase::new(
        PREPARE,
        vec![Step::append(
            "disable mdadm auto-assembly during install",
            "/etc/mdadm/mdadm.conf",
            "AUTO -all\n",
        )],
    )
}

pub fn partition_phase(cfg: &Config, layout: &Layout, stack: &dyn Stack) -> Phase {
    let mut s = vec![Step::run_owned("wipe filesystem signatures", {
        let mut a = strs(&["wipefs", "-a"]);
        a.extend(layout.members.iter().map(|d| format!("/dev/{d}")));
        a
    })];
    s.extend(partition_disks(layout, cfg.install.boot_mode == "efi"));
    s.extend(stack.partition_root(cfg, layout));
    Phase::new(PARTITION, s)
}

/// gpt + esp/boot partitions on each member of the given layout.
fn partition_disks(layout: &Layout, efi: bool) -> Vec<Step> {
    let mut s = Vec::new();
    for d in &layout.members {
        let dev = format!("/dev/{d}");
        s.push(Step::run_owned(
            format!("zap gpt on {dev}"),
            strs(&["sgdisk", "--zap-all"])
                .into_iter()
                .chain([dev.clone()])
                .collect(),
        ));
        if efi {
            s.push(sgdisk(&dev, "create esp", "-n1:1M:+512M", "-t1:EF00"));
        } else {
            s.push(sgdisk(&dev, "create bios-boot", "-n1:1M:+16M", "-t1:EF02"));
        }
        s.push(sgdisk(
            &dev,
            "create boot member",
            "-n2:0:+512M",
            "-t2:8301",
        ));
    }
    s
}

fn sgdisk(dev: &str, note: &str, size: &str, type_: &str) -> Step {
    Step::run_owned(
        format!("{note} on {dev}"),
        vec![
            "sgdisk".to_string(),
            size.to_string(),
            type_.to_string(),
            dev.to_string(),
        ],
    )
}

pub fn format_phase(cfg: &Config, layout: &Layout, stack: &dyn Stack) -> Phase {
    let boot = layout.boot_devices();
    let mut s = vec![Step::run_owned(
        "zero stale md superblocks on boot members",
        {
            let mut a = strs(&["mdadm", "--zero-superblock"]);
            a.extend(boot.iter().cloned());
            a
        },
    )];
    if layout.boot_raid() {
        s.push(stack::md_create(
            BOOT_MD_NAME,
            &cfg.boot.level.to_string(),
            &cfg.boot.bitmap,
            &boot,
        ));
        s.push(Step::run(
            "format /boot as ext4",
            &["mkfs.ext4", "-m", "0", BOOT_MD_DEVICE],
        ));
    } else {
        s.extend(format_boot_independent(&boot));
    }
    if cfg.install.boot_mode == "efi" {
        for dev in layout.esp_devices() {
            s.push(Step::run_owned(
                format!("format esp {dev}"),
                strs(&["mkfs.msdos", "-F", "32", "-s", "1", "-n", "EFI"])
                    .into_iter()
                    .chain([dev])
                    .collect(),
            ));
        }
    }
    s.extend(stack.format_root(cfg, layout));
    Phase::new(FORMAT, s)
}

/// format each member's boot partition as an independent ext4 /boot, all sharing
/// the first member's fs uuid so every disk's grub finds its own local copy.
fn format_boot_independent(boot: &[String]) -> Vec<Step> {
    // -F so mke2fs overwrites a residual fs non-interactively (a re-used disk or
    // a partition whose old superblock survived repartitioning would else prompt).
    let src = boot[0].clone();
    let mut s = vec![Step::run_owned(
        format!("format {src} as ext4 (/boot)"),
        strs(&["mkfs.ext4", "-m", "0", "-F", "-L", "boot"])
            .into_iter()
            .chain([src.clone()])
            .collect(),
    )];
    s.extend(boot[1..].iter().map(|dev| {
        Step::sh(
            format!("format {dev} as ext4 sharing the /boot uuid"),
            format!("u=$(blkid -s UUID -o value {src}); mkfs.ext4 -m 0 -F -U \"$u\" -L boot {dev}"),
        )
    }));
    s
}

pub fn bind_phase(cfg: &Config, layout: &Layout) -> Phase {
    // debootstrap already creates these, but mkdir -p keeps bind robust.
    let mut s = vec![Step::run(
        "create bind mount points",
        &[
            "mkdir",
            "-p",
            "/mnt/dev",
            "/mnt/proc",
            "/mnt/sys",
            "/mnt/run",
        ],
    )];
    s.extend(["/dev", "/proc", "/sys", "/run"].iter().map(|p| {
        Step::run_owned(
            format!("bind {p} into the target"),
            vec![
                "mount".to_string(),
                "--rbind".to_string(),
                p.to_string(),
                format!("/mnt{p}"),
            ],
        )
    }));
    s.push(Step::run("create /mnt/boot", &["mkdir", "-p", "/mnt/boot"]));
    if layout.boot_raid() {
        s.push(Step::run(
            "mount /boot",
            &["mount", BOOT_MD_DEVICE, "/mnt/boot"],
        ));
    } else {
        // mount the first member's independent /boot; every copy shares one uuid.
        let dev = layout.boot_devices().into_iter().next().unwrap_or_default();
        s.push(Step::run_owned(
            "mount /boot",
            vec!["mount".to_string(), dev, "/mnt/boot".to_string()],
        ));
    }
    if cfg.install.boot_mode == "efi" {
        let primary = layout.esp_primary(); // /boot/efi1
        s.push(Step::run_owned(
            format!("create {primary} mount point"),
            vec![
                "mkdir".to_string(),
                "-p".to_string(),
                format!("/mnt{primary}"),
            ],
        ));
        let esp0 = layout.esp_devices().into_iter().next().unwrap_or_default();
        s.push(Step::run_owned(
            "mount the primary esp",
            vec!["mount".to_string(), esp0, format!("/mnt{primary}")],
        ));
        // /boot/efi is a relative symlink to the active esp so grub-install and
        // the mirror hook share one stable path; re-point it to fail over.
        let target = primary.trim_start_matches("/boot/").to_string(); // efi1
        s.push(Step::run_owned(
            format!("link {} -> {target}", ESP_LINK),
            vec![
                "ln".to_string(),
                "-sfn".to_string(),
                target,
                format!("/mnt{ESP_LINK}"),
            ],
        ));
    }
    Phase::new(BIND, s)
}

pub fn strap_phase(cfg: &Config) -> Phase {
    Phase::new(
        STRAP,
        vec![Step::run_owned(
            format!("debootstrap {} into /mnt", cfg.install.release),
            vec![
                "debootstrap".to_string(),
                cfg.install.release.clone(),
                "/mnt".to_string(),
            ],
        )],
    )
}

pub fn install_base_phase(cfg: &Config, stack: &dyn Stack, auto_root_password: bool) -> Phase {
    let mut s = vec![Step::sh(
        "enable contrib in the target",
        "sed -i 's/ main/ main contrib/g' /mnt/etc/apt/sources.list",
    )];
    if cfg.install.backports {
        s.push(Step::write(
            "enable backports in the target",
            "/mnt/etc/apt/sources.list.d/backports.list",
            format!(
                "deb http://deb.debian.org/debian/ {}-backports main contrib\n",
                cfg.install.release
            ),
        ));
        if let Some(pins) = stack.backports_pins(&cfg.install.release) {
            s.push(Step::write(
                "pin backports packages",
                "/mnt/etc/apt/preferences.d/backports",
                pins,
            ));
        }
    }
    s.push(Step::run_owned("refresh package lists in target", apt_get(&["update"])).chroot());
    s.extend(stack.apt_repos(cfg, "/mnt"));
    // dosfstools (mkfs.msdos) is needed on the running system, not just the host:
    // replace recreates a replacement disk's esp there. gdisk/mdadm likewise.
    let mut base = strs(&["gdisk", "dosfstools", "mdadm"]);
    base.extend(stack.packages());
    s.push(apt_install("install raid packages in target", &base).chroot());
    if !cfg.install.extra_packages.is_empty() {
        s.push(
            apt_install(
                "install extra packages in target",
                &cfg.install.extra_packages,
            )
            .chroot(),
        );
    }
    s.push(apt_install("install locales in target", &strs(&["locales"])).chroot());
    s.push(
        apt_install(
            "install the kernel in target",
            &strs(&["linux-image-amd64"]),
        )
        .chroot(),
    );
    s.push(Step::sh(
        "configure dhcp networking",
        "mkdir -p /etc/network/interfaces.d; e=$(ip addr show | awk '/inet.*brd/{print $NF; exit}'); [ -n \"$e\" ] || e=enp0s1; printf '\\nauto %s\\niface %s inet dhcp\\n' \"$e\" \"$e\" >> /etc/network/interfaces.d/$e",
    ).chroot());
    s.push(
        Step::run_owned(
            "upgrade the base system",
            apt_get(&["full-upgrade", "-y", "--autoremove", "--purge"]),
        )
        .chroot(),
    );
    s.push(apt_install("install tasksel in target", &strs(&["tasksel"])).chroot());
    s.push(
        Step::run(
            "install the standard task",
            &[
                "env",
                "DEBIAN_FRONTEND=noninteractive",
                "tasksel",
                "--debconf-apt-progress=--logstderr",
                "install",
                "standard",
            ],
        )
        .chroot(),
    );
    if auto_root_password {
        // chpasswd reads "root:<password>"; the prefix is added at run time.
        s.push(
            Step::run(
                "set the root password (same as the disk password)",
                &["chpasswd"],
            )
            .chroot()
            .secret_prefixed("root:"),
        );
    } else {
        s.push(Step::run("set the root password", &["passwd"]).chroot());
    }
    Phase::new(INSTALL, s)
}

pub fn bootloader_phase(cfg: &Config, layout: &Layout) -> Phase {
    let efi = cfg.install.boot_mode == "efi";
    let mut s =
        vec![Step::run("remove os-prober", &["apt-get", "purge", "-y", "os-prober"]).chroot()];
    // rsync is needed in the target for the esp-mirror grub.d hook (efi) and for
    // the boot-mirror sync (independent /boot); update-grub runs both at the end
    // of this phase, in this same chroot.
    let mut pkgs = if efi {
        strs(&["grub-efi-amd64", "shim-signed"])
    } else {
        strs(&["grub-pc"])
    };
    if efi || !layout.boot_raid() {
        pkgs.push("rsync".to_string());
    }
    s.push(apt_install("install grub + mirror packages in target", &pkgs).chroot());
    s.push(boot_fstab_step(layout));
    if efi {
        s.extend(bootloader_efi(layout));
    } else {
        // preseed grub-pc's install devices so it configures non-interactively: an
        // interactive `dpkg-reconfigure grub-pc` hangs the unattended install on a
        // debconf dialog. then install grub to each member's mbr explicitly (the
        // authoritative install); the preseed also keeps future grub-pc upgrades
        // from prompting.
        let devices = layout
            .members
            .iter()
            .map(|d| format!("/dev/{d}"))
            .collect::<Vec<_>>()
            .join(", ");
        s.push(Step::sh(
            "preseed grub-pc install devices",
            format!(
                "printf 'grub-pc grub-pc/install_devices multiselect {devices}\\n' | chroot /mnt debconf-set-selections"
            ),
        ));
        for d in &layout.members {
            s.push(
                Step::run_owned(
                    format!("install grub to {d}"),
                    vec!["grub-install".to_string(), format!("/dev/{d}")],
                )
                .chroot(),
            );
        }
    }
    if !layout.boot_raid() {
        s.extend(boot_mirror_install_steps(layout));
    }
    if cfg.install.serial_console {
        s.extend(serial_console_steps());
    }
    s.push(Step::run("regenerate grub config", &["update-grub"]).chroot());
    // the initial boot-mirror content sync happens in the finish phase, after the
    // crypttab-aware initrd is built -- see boot_mirror_sync_step.
    Phase::new(BOOTLOADER, s)
}

/// strict one-shot sync of the boot mirrors. used at install time after the final
/// initrd exists; the kernel hooks handle later updates. an earlier sync (before
/// update-initramfs) would ship a cryptsetup-less initrd, so a survivor booted
/// after the primary's /boot is lost could not unlock the encrypted root.
fn boot_mirror_sync_step() -> Step {
    Step::run_owned(
        "sync the boot mirrors",
        vec![
            stack::BOOT_MIRROR_SYNC_PATH.to_string(),
            "--strict".to_string(),
        ],
    )
    .chroot()
}

/// the /boot fstab entry: the md array (raid) or the shared-uuid independent fs.
/// nofail keeps a missing/degraded /boot from dropping boot into emergency.
fn boot_fstab_step(layout: &Layout) -> Step {
    if layout.boot_raid() {
        Step::write(
            "write the boot fstab entry",
            "/mnt/etc/fstab",
            "/dev/md/boot /boot ext4 defaults,nofail 0 2\n",
        )
    } else {
        let dev = layout.boot_devices().into_iter().next().unwrap_or_default();
        Step::sh(
            "write the boot fstab entry",
            format!(
                "uuid=$(blkid -s UUID -o value {dev}); \
                 echo \"UUID=$uuid /boot ext4 defaults,nofail 0 2\" > /mnt/etc/fstab"
            ),
        )
    }
}

/// independent /boot: fstab mirror entries plus the sync script and its kernel
/// hooks. each non-primary member's /boot mounts noauto at /boot.mirrorN by
/// device (so the sync writes each physical disk, not whatever uuid resolves
/// first); the live /boot is mounted by the shared uuid (see boot_fstab_step).
fn boot_mirror_install_steps(layout: &Layout) -> Vec<Step> {
    let mut s = boot_mirror_fstab_steps(layout);
    // the script and hook dirs exist in a base install, but mkdir -p keeps this
    // robust (Step::write does not create parent dirs).
    s.push(Step::run(
        "create the boot mirror script and hook dirs",
        &[
            "mkdir",
            "-p",
            "/mnt/usr/local/sbin",
            "/mnt/etc/kernel/postinst.d",
            "/mnt/etc/kernel/postrm.d",
        ],
    ));
    s.push(Step::write_mode(
        "install the boot mirror sync script",
        format!("/mnt{}", stack::BOOT_MIRROR_SYNC_PATH),
        stack::BOOT_MIRROR_SYNC,
        0o755,
    ));
    // the hooks run after zz-update-grub, so each new kernel/initrd and the
    // regenerated grub.cfg reach every disk's /boot.
    for dir in ["postinst.d", "postrm.d"] {
        s.push(Step::run_owned(
            format!("hook the boot mirror sync into kernel {dir}"),
            vec![
                "ln".to_string(),
                "-sfn".to_string(),
                stack::BOOT_MIRROR_SYNC_PATH.to_string(),
                format!("/mnt/etc/kernel/{dir}/{}", stack::BOOT_MIRROR_HOOK_NAME),
            ],
        ));
    }
    s
}

/// fstab lines for every non-primary member's /boot mirror, addressed by device.
fn boot_mirror_fstab_steps(layout: &Layout) -> Vec<Step> {
    let devices = layout.boot_devices();
    let mounts = layout.boot_mounts();
    devices
        .iter()
        .zip(mounts.iter())
        .skip(1)
        .map(|(dev, mnt)| {
            Step::sh(
                format!("add boot mirror {mnt} to fstab"),
                format!(
                    "mkdir -p /mnt{mnt}; echo \"{dev} {mnt} ext4 noauto,nofail 0 0\" >> /mnt/etc/fstab"
                ),
            )
        })
        .collect()
}

// grub serial config so the installed system's boot menu, kernel console, and
// the initramfs cryptsetup unlock prompt all reach ttyS0.
const GRUB_SERIAL_CFG: &str = "GRUB_CMDLINE_LINUX_DEFAULT=\"console=tty1 console=ttyS0,115200\"\n\
     GRUB_TERMINAL=\"console serial\"\n\
     GRUB_SERIAL_COMMAND=\"serial --speed=115200 --unit=0 --word=8 --parity=no --stop=1\"\n";

fn serial_console_steps() -> Vec<Step> {
    vec![
        Step::write(
            "enable grub serial console",
            "/mnt/etc/default/grub.d/serial.cfg",
            GRUB_SERIAL_CFG,
        ),
        Step::run(
            "enable a login on the serial console",
            &["systemctl", "enable", "serial-getty@ttyS0.service"],
        )
        .chroot(),
    ]
}

fn bootloader_efi(layout: &Layout) -> Vec<Step> {
    let mut s = vec![Step::run(
        "install grub to the esp",
        &[
            "grub-install",
            "--target=x86_64-efi",
            "--bootloader-id=debian",
            "--efi-directory=/boot/efi",
            "--no-nvram",
            "--recheck",
            "--no-floppy",
            "--removable",
        ],
    )
    .chroot()];
    // reverse order so the first disk ends up first in the firmware boot menu.
    for d in layout.members.iter().rev() {
        s.push(
            Step::run_owned(
                format!("register efi boot entry for {d}"),
                vec![
                    "efibootmgr".to_string(),
                    "-c".to_string(),
                    "-g".to_string(),
                    "-d".to_string(),
                    format!("/dev/{d}"),
                    "-p".to_string(),
                    "1".to_string(),
                    "-L".to_string(),
                    format!("debian-{d}"),
                    "-l".to_string(),
                    r"\EFI\debian\shimx64.efi".to_string(),
                ],
            )
            .chroot(),
        );
    }
    s.extend(esp_fstab_steps(layout));
    s.push(Step::write_mode(
        "install the esp mirror grub.d hook",
        "/mnt/etc/grub.d/90_copy_to_efi_mirrors",
        stack::EFI_MIRROR_HOOK,
        0o755,
    ));
    s
}

/// fstab entries for every esp: the first (the /boot/efi symlink target) is the
/// auto primary, the rest are noauto mirrors. each uuid is resolved via blkid.
fn esp_fstab_steps(layout: &Layout) -> Vec<Step> {
    let devices = layout.esp_devices();
    let mounts = layout.esp_mounts();
    devices
        .iter()
        .zip(mounts.iter())
        .enumerate()
        .map(|(i, (dev, mnt))| {
            if i == 0 {
                Step::sh(
                    format!("add {mnt} to fstab"),
                    format!("uuid=$(blkid -s UUID -o value {dev}); echo \"UUID=$uuid {mnt} vfat {EFI_OPTS},nofail 0 0\" >> /mnt/etc/fstab"),
                )
            } else {
                Step::sh(
                    format!("add esp mirror {mnt} to fstab"),
                    format!("mkdir -p /mnt{mnt}; uuid=$(blkid -s UUID -o value {dev}); echo \"UUID=$uuid {mnt} vfat {EFI_OPTS},noauto 0 0\" >> /mnt/etc/fstab"),
                )
            }
        })
        .collect()
}

pub fn close_phase(cfg: &Config, layout: &Layout, stack: &dyn Stack) -> Phase {
    let mut s = vec![Step::sh(
        "unmount everything under /mnt",
        "mount | tac | awk '/\\/mnt/ {print $3}' | xargs -r -I{} umount -lf {}",
    )
    .best_effort()];
    s.extend(stack.close(cfg, layout));
    if layout.boot_raid() {
        s.push(stack::md_stop(BOOT_MD_DEVICE));
    }
    Phase::new(CLOSE, s)
}
