// `raiden recover`: bring a degraded root online from the initramfs so the boot
// can continue. it generalizes the per-stack manual `(initramfs)` commands (eg.
// btrfs `mount -o degraded`) into one command. structured as check/fix like
// doctor: it observes whether the root is already mounted, and if not, runs the
// stack's recovery actions -- each confirmed individually unless --yes. crypt
// members are already open by the initramfs (cryptroot + decrypt_keyctl), so this
// picks up at the array/mount layer. raiden + the manifest are baked into the
// initrd by install (install.initramfs_recovery, default on), so the command is
// available at the rescue shell with neither /boot nor the root mounted.

use anyhow::{bail, Result};

use crate::config::Config;
use crate::layout::Layout;
use crate::prompt;
use crate::stack::{RecoverAction, Stack};

pub fn run(
    cfg: &Config,
    layout: &Layout,
    stack: &dyn Stack,
    at: &str,
    yes: bool,
    dry_run: bool,
    verbose: bool,
) -> Result<()> {
    let actions = stack.recover_actions(cfg, layout, at);
    if dry_run {
        preview(&actions, at);
        return Ok(());
    }
    // check: an already-mounted root means the boot is recoverable as-is.
    if is_mounted(at) {
        println!("root already mounted at {at}; nothing to recover");
        return Ok(());
    }
    for action in &actions {
        if !prompt::confirm_or_yes(yes, &format!("{}?", action.label))? {
            eprintln!("  skipped: {}", action.label);
            continue;
        }
        eprintln!("  - {}", action.label);
        for step in &action.steps {
            if verbose {
                for line in step.describe() {
                    eprintln!("    {line}");
                }
            }
            // a recovery step is judged by the postcondition (is the root mounted),
            // not its own exit: assemble/import steps are best-effort and a mount
            // that lost the race to a sibling member is harmless. log and continue.
            if let Err(e) = step.execute(None) {
                eprintln!("    ! {e}");
            }
        }
    }
    // postcondition: the boot can only continue if the root actually mounted.
    if is_mounted(at) {
        eprintln!("\nrecover: root mounted at {at}; exit the shell to continue booting");
        Ok(())
    } else {
        bail!("recover: root is still not mounted at {at}");
    }
}

/// print the recovery flow (each action and the exact commands it would run, in
/// order) without touching anything. the look-before-you-leap preview, and the
/// pure path the hermetic tests exercise (no /proc/mounts read, no execution).
fn preview(actions: &[RecoverAction], at: &str) {
    println!("# recovery flow: bring the root online at {at}\n");
    for (i, action) in actions.iter().enumerate() {
        println!("[{}] {}", i + 1, action.label);
        for step in &action.steps {
            for line in step.describe() {
                println!("    {line}");
            }
        }
        println!();
    }
}

/// whether `path` is currently a mount point, read from /proc/mounts (no external
/// tool, so it works in the bare initramfs where findmnt may be absent).
fn is_mounted(path: &str) -> bool {
    std::fs::read_to_string("/proc/mounts")
        .map(|s| s.lines().any(|l| l.split_whitespace().nth(1) == Some(path)))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stack;

    fn actions_for(stack_id: &str, members: &[&str]) -> Vec<RecoverAction> {
        let mut cfg = Config::default();
        cfg.raid.stack = stack_id.to_string();
        cfg.disks.members = members.iter().map(|s| s.to_string()).collect();
        let layout = Layout::derive(&cfg);
        let s = stack::select(stack_id).unwrap();
        s.recover_actions(&cfg, &layout, "/root")
    }

    /// the flattened command lines of an action set, for asserting content.
    fn commands(actions: &[RecoverAction]) -> Vec<String> {
        actions
            .iter()
            .flat_map(|a| a.steps.iter())
            .flat_map(|s| s.describe())
            .collect()
    }

    #[test]
    fn md_recovers_by_running_the_array_and_mounting_lvm() {
        let cmds = commands(&actions_for(
            crate::config::STACK_MD_LVM_EXT4,
            &["vda", "vdb"],
        ));
        let joined = cmds.join("\n");
        assert!(joined.contains("mdadm --run /dev/md/root"));
        assert!(joined.contains("mount /dev/vg0/root /root"));
    }

    #[test]
    fn btrfs_recovers_with_a_degraded_mount_at_the_target() {
        let cmds = commands(&actions_for(crate::config::STACK_BTRFS, &["vda", "vdb"]));
        let joined = cmds.join("\n");
        assert!(joined.contains("device scan"));
        // a degraded mount from a crypt member, at the requested target.
        assert!(joined.contains("mount -o degraded"));
        assert!(joined.contains("/dev/mapper/vda3_crypt"));
        // no `mountpoint`: it is absent in the initramfs (the step would log a
        // spurious failure), and recover already verifies via is_mounted.
        assert!(!joined.contains("mountpoint"));
    }

    #[test]
    fn bcachefs_recovers_with_a_degraded_mount_without_mountpoint() {
        let cmds = commands(&actions_for(crate::config::STACK_BCACHEFS, &["vda", "vdb"]));
        let joined = cmds.join("\n");
        assert!(joined.contains("-o degraded"));
        assert!(!joined.contains("mountpoint"));
    }

    #[test]
    fn zfs_recovers_by_force_importing_then_mounting_the_dataset() {
        let cmds = commands(&actions_for(crate::config::STACK_ZFS, &["vda", "vdb"]));
        let joined = cmds.join("\n");
        assert!(joined.contains("zpool import -f -N -R /root rpool"));
        assert!(joined.contains("zfs mount rpool/ROOT/debian"));
    }
}
