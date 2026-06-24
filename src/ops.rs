// post-install operations, assembled from the stack methods and the reusable
// pipeline phase builders. all return a plan (ordered phases) so they share the
// same dry-run, execution, and resume machinery as install.

use anyhow::{bail, Result};

use crate::config::Config;
use crate::layout::{Layout, BOOT_MD_DEVICE};
use crate::pipeline;
use crate::stack::Stack;
use crate::step::{Phase, Step};

const SHIM: &str = r"\EFI\debian\shimx64.efi";

/// assemble + unlock + mount from a livecd, after first installing tools and
/// tearing down any partial state.
pub fn rescue(cfg: &Config, layout: &Layout, stack: &dyn Stack) -> Vec<Phase> {
    vec![
        pipeline::apt_phase(cfg, stack),
        pipeline::prepare_phase(),
        pipeline::close_phase(cfg, layout, stack),
        Phase::new("map", stack.map(cfg, layout)),
        Phase::new("mount", stack.mount_root(cfg, layout)),
        pipeline::bind_phase(cfg, layout),
    ]
}

pub fn close(cfg: &Config, layout: &Layout, stack: &dyn Stack) -> Vec<Phase> {
    vec![pipeline::close_phase(cfg, layout, stack)]
}

/// ensure the stack is open and mounted. the full form (under /mnt) opens crypt,
/// assembles the array, activates lvm, and mounts root plus /boot and /boot/efi;
/// `boot_only` mounts just /boot and /boot/efi under `at` (no crypt, no password).
/// every step is guarded, so it is safe to run against an already-up system.
pub fn mount(
    cfg: &Config,
    layout: &Layout,
    stack: &dyn Stack,
    boot_only: bool,
    at: &str,
) -> Vec<Phase> {
    let efi = cfg.install.boot_mode == "efi";
    let mut s = Vec::new();
    if !boot_only {
        s.extend(stack.map(cfg, layout));
        s.extend(stack.mount_root(cfg, layout));
    }
    s.extend(pipeline::boot_mount_steps(layout, at, efi));
    vec![Phase::new("mount", s)]
}

pub fn scrub(cfg: &Config, layout: &Layout, stack: &dyn Stack) -> Vec<Phase> {
    // independent /boot is a plain ext4 per disk with no array to scrub.
    let mut s = Vec::new();
    if layout.boot_raid() {
        s.push(
            Step::run(
                "start a check scrub on /boot",
                &["mdadm", "--action=check", BOOT_MD_DEVICE],
            )
            .best_effort(),
        );
        s.push(
            Step::run(
                "wait for the boot scrub",
                &["mdadm", "--wait", BOOT_MD_DEVICE],
            )
            .best_effort(),
        );
    }
    s.extend(stack.scrub(cfg, layout));
    vec![Phase::new("scrub", s)]
}

pub fn remove(
    cfg: &Config,
    layout: &Layout,
    stack: &dyn Stack,
    disks: &[String],
) -> Result<Vec<Phase>> {
    check_members(layout, disks)?;
    let mut s = boot_detach(layout, disks);
    s.extend(stack.remove(cfg, layout, disks));
    Ok(vec![Phase::new("remove", s)])
}

/// which per-disk layers a replace rebuilds. no layer flag means all of them (the
/// whole disk); naming layers rebuilds only those -- eg. `--esp --boot` rebuilds
/// the boot region without touching the root member, so there is no resilver.
pub struct ReplaceParts {
    pub esp: bool,
    pub boot: bool,
    pub root: bool,
}

impl ReplaceParts {
    pub fn from_flags(esp: bool, boot: bool, root: bool) -> Self {
        if !esp && !boot && !root {
            Self {
                esp: true,
                boot: true,
                root: true,
            }
        } else {
            Self { esp, boot, root }
        }
    }

    fn full(&self) -> bool {
        self.esp && self.boot && self.root
    }

    /// the member partition numbers selected (1=esp, 2=boot, 3=root).
    fn part_numbers(&self) -> Vec<u32> {
        [(self.esp, 1u32), (self.boot, 2), (self.root, 3)]
            .into_iter()
            .filter_map(|(sel, n)| sel.then_some(n))
            .collect()
    }
}

/// rebuild the named disks' selected layers: detach, repartition, re-add to the
/// arrays, and repopulate boot/esp. a full (default) replace rebuilds the whole
/// disk; a partial one (eg. `--esp --boot`) rebuilds only those partitions in
/// place and leaves the root member -- and its array -- untouched, so it skips the
/// (slow) resilver.
pub fn replace(
    cfg: &Config,
    layout: &Layout,
    stack: &dyn Stack,
    disks: &[String],
    parts: &ReplaceParts,
) -> Result<Vec<Phase>> {
    check_members(layout, disks)?;
    let healthy: Vec<String> = layout
        .members
        .iter()
        .filter(|m| !disks.contains(m))
        .cloned()
        .collect();
    if healthy.is_empty() {
        bail!("cannot replace every disk at once; at least one member must survive to clone from");
    }
    let efi = cfg.install.boot_mode == "efi";

    // remove: detach the selected layers and wipe their partitions.
    let mut rm = Vec::new();
    if parts.boot {
        rm.extend(boot_detach(layout, disks));
        // independent /boot mounts by the shared uuid, so the live /boot may sit on
        // a disk we are about to rebuild; move it to a survivor first.
        if !layout.boot_raid() {
            rm.push(relocate_boot_step(layout, disks, &healthy[0]));
        }
    }
    if parts.root {
        rm.extend(stack.remove(cfg, layout, disks));
    }
    if parts.esp {
        // the primary esp is mounted at /boot/efi on a healthy system; unmount the
        // esp of any disk being rebuilt before wipefs/mkfs touch it, or mkfs.msdos
        // refuses ("contains a mounted filesystem"). best-effort: a mirror esp or a
        // destroyed primary is not mounted.
        for d in disks {
            let esp = layout.part(d, 1);
            rm.push(
                Step::run_owned(
                    format!("unmount {esp} if mounted"),
                    vec!["umount".to_string(), esp],
                )
                .best_effort(),
            );
        }
    }
    // udev releases the just-closed crypt/dm devices asynchronously; settle before
    // wiping and repartitioning so the freed partitions are not still "busy".
    rm.push(Step::run("settle udev after teardown", &["udevadm", "settle"]).best_effort());
    for d in disks {
        for p in parts.part_numbers() {
            let dev = layout.part(d, p);
            rm.push(
                Step::run_owned(
                    format!("wipe {dev}"),
                    vec!["wipefs".to_string(), "-a".to_string(), dev],
                )
                .best_effort(),
            );
        }
    }

    // partition: recreate the selected partitions (full = zap + recreate all;
    // partial = recreate just those in place), then bring up the root layer.
    let mut pt = partition_replacements(layout, disks, efi, parts);
    if parts.root {
        pt.extend(stack.partition_root(cfg, &layout.subset(disks)));
    }

    // reassemble: re-add the selected boot/root members and resilver. settle first
    // so udev has finished probing the just-recreated partitions.
    let mut re =
        vec![Step::run("settle udev after repartitioning", &["udevadm", "settle"]).best_effort()];
    if parts.boot {
        if layout.boot_raid() {
            re.extend(disks.iter().map(|d| {
                let bdev = layout.part(d, 2);
                Step::run_owned(
                    format!("add {bdev} to the boot array"),
                    vec![
                        "mdadm".to_string(),
                        "--add".to_string(),
                        BOOT_MD_DEVICE.to_string(),
                        bdev,
                    ],
                )
            }));
        } else {
            // no array: re-create each replaced disk's /boot with the shared uuid
            // (so its grub finds a local copy); content is cloned in the bootloader
            // phase.
            let src = layout.part(&healthy[0], 2);
            re.extend(disks.iter().map(|d| {
                let bdev = layout.part(d, 2);
                Step::sh(
                    format!("format {bdev} as ext4 sharing the /boot uuid"),
                    format!(
                        "u=$(blkid -s UUID -o value {src}); mkfs.ext4 -m 0 -F -U \"$u\" -L boot {bdev}"
                    ),
                )
            }));
        }
    }
    if parts.root {
        re.extend(stack.replace(cfg, layout, disks));
    }
    if parts.boot && layout.boot_raid() {
        // stack.replace already waits for the root array; wait for the boot array
        // too. returning with /boot mid-rebuild leaves it unreadable by grub.
        re.push(
            Step::run(
                "wait for the boot array rebuild",
                &["mdadm", "--wait", BOOT_MD_DEVICE],
            )
            .best_effort(),
        );
    }

    let mut phases = vec![
        Phase::new("remove", rm),
        Phase::new("partition", pt),
        Phase::new("reassemble", re),
    ];
    let mut boot = repopulate_boot(layout, disks, &healthy[0], efi, parts);
    // the esp was unmounted for the rebuild; remount /boot + /boot/efi from the
    // first available member so the running system is left consistent (idempotent;
    // a no-op for whatever is already mounted).
    if parts.esp || parts.boot {
        boot.extend(pipeline::boot_mount_steps(layout, "/", efi));
    }
    if !boot.is_empty() {
        phases.push(Phase::new("bootloader", boot));
    }
    Ok(phases)
}

/// read-only health report steps (boot array detail + the stack's native
/// status). the md read-error-to-file mapping is run separately by the caller.
pub fn status_steps(cfg: &Config, layout: &Layout, stack: &dyn Stack) -> Vec<Step> {
    let mut s = vec![if layout.boot_raid() {
        Step::run("boot array detail", &["mdadm", "--detail", BOOT_MD_DEVICE]).best_effort()
    } else {
        Step::run("boot mount", &["findmnt", "/boot"]).best_effort()
    }];
    s.extend(stack.status(cfg, layout));
    s
}

fn check_members(layout: &Layout, disks: &[String]) -> Result<()> {
    if disks.is_empty() {
        bail!("no disks specified");
    }
    for d in disks {
        if !layout.members.contains(d) {
            bail!("{d:?} is not a configured member disk");
        }
    }
    Ok(())
}

/// fail and remove each disk's boot member from the boot array (best-effort).
/// independent /boot has no array, so there is nothing to detach.
fn boot_detach(layout: &Layout, disks: &[String]) -> Vec<Step> {
    if !layout.boot_raid() {
        return Vec::new();
    }
    let mut s = Vec::new();
    for d in disks {
        let bdev = layout.part(d, 2);
        s.push(
            Step::run_owned(
                format!("fail {bdev} in the boot array"),
                vec![
                    "mdadm".to_string(),
                    "--fail".to_string(),
                    BOOT_MD_DEVICE.to_string(),
                    bdev.clone(),
                ],
            )
            .best_effort(),
        );
        s.push(
            Step::run_owned(
                format!("remove {bdev} from the boot array"),
                vec![
                    "mdadm".to_string(),
                    "--remove".to_string(),
                    BOOT_MD_DEVICE.to_string(),
                    bdev,
                ],
            )
            .best_effort(),
        );
    }
    // clear any slot left behind by a wholly-lost disk (no device node to target).
    s.extend(crate::stack::md_drop_missing(BOOT_MD_DEVICE));
    s
}

/// (independent /boot) if the live /boot is mounted from a disk being replaced,
/// remount it from a survivor so the replaced disk's boot partition can be
/// reformatted -- mkfs refuses a mounted device, and /boot mounts by the shared
/// uuid so it can land on any disk. a no-op when /boot is already on a survivor.
fn relocate_boot_step(layout: &Layout, disks: &[String], survivor: &str) -> Step {
    let replaced = disks
        .iter()
        .map(|d| layout.part(d, 2))
        .collect::<Vec<_>>()
        .join(" ");
    let survivor_boot = layout.part(survivor, 2);
    Step::sh(
        "move /boot off any disk being replaced",
        format!(
            "src=$(findmnt -no SOURCE /boot 2>/dev/null || true); \
             for r in {replaced}; do \
             if [ \"$src\" = \"$r\" ]; then umount -R /boot && mount {survivor_boot} /boot; break; fi; \
             done"
        ),
    )
}

fn partition_replacements(
    layout: &Layout,
    disks: &[String],
    efi: bool,
    parts: &ReplaceParts,
) -> Vec<Step> {
    let mut s = Vec::new();
    let esp_part = |dev: &str| {
        if efi {
            sgdisk(dev, "create esp", "-n1:1M:+512M", "-t1:EF00")
        } else {
            sgdisk(dev, "create bios-boot", "-n1:1M:+16M", "-t1:EF02")
        }
    };
    for d in disks {
        let dev = format!("/dev/{d}");
        if parts.full() {
            s.push(Step::run_owned(
                format!("zap gpt on {dev}"),
                vec!["sgdisk".to_string(), "--zap-all".to_string(), dev.clone()],
            ));
            s.push(esp_part(&dev));
            s.push(sgdisk(
                &dev,
                "create boot member",
                "-n2:0:+512M",
                "-t2:8301",
            ));
        } else {
            // partial: recreate only the selected partitions in place, leaving the
            // gpt and the unselected partitions (and their uuids) untouched.
            if parts.esp {
                s.push(sgdisk_delete(&dev, 1));
                s.push(esp_part(&dev));
            }
            if parts.boot {
                s.push(sgdisk_delete(&dev, 2));
                s.push(sgdisk(
                    &dev,
                    "create boot member",
                    "-n2:0:+512M",
                    "-t2:8301",
                ));
            }
            if parts.root {
                // stack.partition_root recreates p3; just clear the old one first.
                s.push(sgdisk_delete(&dev, 3));
            }
        }
    }
    if efi && parts.esp {
        for d in disks {
            let esp = layout.part(d, 1);
            if layout.esp_is_primary(d) {
                // the primary esp is the one in fstab at /boot/efi; keep its uuid
                // so that entry stays valid. the others are mirrors (no fstab
                // entry, fresh uuid) repopulated from a survivor by the esp hook.
                s.push(Step::sh(
                    format!("recreate primary esp on {esp} preserving its uuid"),
                    format!(
                        "uuid=$(awk '$2==\"/boot/efi\" {{print $1}}' /etc/fstab | sed 's/^UUID=//'); \
                         if [ -n \"$uuid\" ]; then mkfs.msdos -F 32 -s 1 -n EFI -i \"$(echo $uuid | tr -d -)\" {esp}; \
                         else mkfs.msdos -F 32 -s 1 -n EFI {esp}; fi"
                    ),
                ));
            } else {
                s.push(Step::run_owned(
                    format!("recreate esp on {esp}"),
                    vec![
                        "mkfs.msdos".to_string(),
                        "-F".to_string(),
                        "32".to_string(),
                        "-s".to_string(),
                        "1".to_string(),
                        "-n".to_string(),
                        "EFI".to_string(),
                        esp,
                    ],
                ));
            }
        }
    }
    s
}

/// repopulate the rebuilt boot layers: clone the esp from a survivor and register
/// its firmware boot entry (efi, when the esp was rebuilt), reinstall grub (bios,
/// when the bios-boot or /boot partition was rebuilt), and clone the independent
/// /boot (when it was rebuilt).
fn repopulate_boot(
    layout: &Layout,
    disks: &[String],
    healthy: &str,
    efi: bool,
    parts: &ReplaceParts,
) -> Vec<Step> {
    let mut s = Vec::new();
    if efi && parts.esp {
        let src = layout.part(healthy, 1);
        for d in disks {
            let dst = layout.part(d, 1);
            s.push(Step::sh(
                format!("clone esp from {src} to {dst}"),
                format!(
                    "x=$(mktemp -d); y=$(mktemp -d); mount -o ro {src} \"$x\"; mount {dst} \"$y\"; \
                     rsync --times --recursive --delete \"$x\"/ \"$y\"/; \
                     umount \"$y\"; rmdir \"$y\"; umount \"$x\"; rmdir \"$x\""
                ),
            ));
            s.push(Step::run_owned(
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
                    SHIM.to_string(),
                ],
            ));
        }
    } else if !efi && (parts.esp || parts.boot) {
        // bios: the bootloader spans the bios-boot partition (p1) and /boot (p2),
        // so reinstall grub if either was rebuilt.
        for d in disks {
            s.push(Step::run_owned(
                format!("install grub to {d}"),
                vec!["grub-install".to_string(), format!("/dev/{d}")],
            ));
        }
    }
    // independent /boot: clone the live /boot onto each rebuilt boot partition so
    // its (now bootable) grub has a local copy.
    if parts.boot && !layout.boot_raid() {
        for d in disks {
            let dst = layout.part(d, 2);
            s.push(Step::sh(
                format!("clone /boot to {dst}"),
                format!(
                    "y=$(mktemp -d); mount {dst} \"$y\"; \
                     rsync --one-file-system --times --recursive --delete /boot/ \"$y\"/; \
                     umount \"$y\"; rmdir \"$y\""
                ),
            ));
        }
    }
    s
}

/// delete a single partition by number, for a partial (in-place) replace.
fn sgdisk_delete(dev: &str, n: u32) -> Step {
    Step::run_owned(
        format!("delete partition {n} on {dev}"),
        vec!["sgdisk".to_string(), format!("-d{n}"), dev.to_string()],
    )
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
