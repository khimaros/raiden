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

/// rebuild the named disks: detach, repartition them, re-add to the arrays, and
/// repopulate their esps.
pub fn replace(
    cfg: &Config,
    layout: &Layout,
    stack: &dyn Stack,
    disks: &[String],
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
    let repl = layout.subset(disks);

    // remove: detach from both arrays, tear down mappings, wipe the disks.
    let mut rm = boot_detach(layout, disks);
    // independent /boot mounts by the shared uuid, so the live /boot may sit on a
    // disk we are about to repartition; move it to a survivor first.
    if !layout.boot_raid() {
        rm.push(relocate_boot_step(layout, disks, &healthy[0]));
    }
    rm.extend(stack.remove(cfg, layout, disks));
    // udev releases the just-closed crypt/dm devices asynchronously; settle before
    // wiping and repartitioning so the freed partitions are not still "busy" at
    // wipefs/mkfs/luksFormat (an intermittent failure when replacing a member
    // whose root layer was still healthy, eg. after losing only its esp+boot).
    rm.push(Step::run("settle udev after teardown", &["udevadm", "settle"]).best_effort());
    for d in disks {
        for p in [1, 2, 3] {
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

    // partition: recreate gpt + esp/boot on the replacements, preserving esp
    // uuid so baked fstab entries stay valid, then bring up the root layer.
    let mut pt = partition_replacements(layout, disks, efi);
    pt.extend(stack.partition_root(cfg, &repl));

    // reassemble: re-add boot members, then re-add root members and resilver.
    // settle first so udev has finished probing the just-recreated partitions
    // (otherwise the immediate --add can hit a transient "busy").
    let mut re =
        vec![Step::run("settle udev after repartitioning", &["udevadm", "settle"]).best_effort()];
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
        // no array: re-create each replaced disk's /boot with the shared uuid (so
        // its grub finds a local copy); content is cloned in the bootloader phase.
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
    re.extend(stack.replace(cfg, layout, disks));
    if layout.boot_raid() {
        // stack.replace already waits for the root array; wait for the boot array
        // too. returning with /boot mid-rebuild leaves it unreadable by grub (and
        // unsurvivable to a further fault) until resync silently finishes later.
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
    let boot = repopulate_boot(layout, disks, &healthy[0], efi);
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

fn partition_replacements(layout: &Layout, disks: &[String], efi: bool) -> Vec<Step> {
    let mut s = Vec::new();
    for d in disks {
        let dev = format!("/dev/{d}");
        s.push(Step::run_owned(
            format!("zap gpt on {dev}"),
            vec!["sgdisk".to_string(), "--zap-all".to_string(), dev.clone()],
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
    if efi {
        for d in disks {
            if let Some(mnt) = layout.esp_mount_of(d) {
                let esp = layout.part(d, 1);
                s.push(Step::sh(
                    format!("recreate esp on {esp} preserving its uuid"),
                    format!(
                        "uuid=$(awk -v m={mnt} '$2==m {{print $1}}' /etc/fstab | sed 's/^UUID=//'); \
                         if [ -n \"$uuid\" ]; then mkfs.msdos -F 32 -s 1 -n EFI -i \"$(echo $uuid | tr -d -)\" {esp}; \
                         else mkfs.msdos -F 32 -s 1 -n EFI {esp}; fi"
                    ),
                ));
            }
        }
    }
    s
}

/// repopulate each replaced disk's esp by cloning a surviving one, then register
/// its firmware boot entry (efi) or reinstall grub (bios).
fn repopulate_boot(layout: &Layout, disks: &[String], healthy: &str, efi: bool) -> Vec<Step> {
    let mut s = Vec::new();
    if efi {
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
    } else {
        for d in disks {
            s.push(Step::run_owned(
                format!("install grub to {d}"),
                vec!["grub-install".to_string(), format!("/dev/{d}")],
            ));
        }
    }
    // independent /boot: clone the live /boot onto each replaced disk's freshly
    // formatted boot partition so its (now bootable) grub has a local copy.
    if !layout.boot_raid() {
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
