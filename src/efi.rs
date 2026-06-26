// the shared EFI bootloader surface. the per-disk boot setup -- the shim path, the
// efibootmgr entry, and grub's debconf -- is established by install (pipeline) and
// replace (ops), verified by doctor, and repaired by doctor --fix. keeping the one
// canonical form of each here stops those three sites from drifting apart.

use crate::step::Step;

// the shim loader path as the firmware records a boot entry (backslash-separated).
pub const SHIM_LOADER: &str = r"\EFI\debian\shimx64.efi";
// shim and grub on the esp as filesystem-relative paths (for fs reads / `test -e`).
pub const SHIM_FILE: &str = "EFI/debian/shimx64.efi";
pub const GRUB_FILE: &str = "EFI/debian/grubx64.efi";

// the grub-efi package and the debconf keys raiden owns: keep the EFI/BOOT removable
// fallback in sync on upgrades, but leave grub's own nvram management off (raiden
// registers the per-disk entries itself). the single source of truth for the
// install preseed and doctor's debconf check/fix.
pub const GRUB_PKG: &str = "grub-efi-amd64";
pub const GRUB_DEBCONF: &[(&str, &str)] = &[
    ("grub2/force_efi_extra_removable", "true"),
    ("grub2/update_nvram", "false"),
];

/// the efibootmgr argv that registers one member disk's per-disk shim boot entry:
/// `efibootmgr -c -g -d /dev/<disk> -p 1 -L debian-<disk> -l <shim>`. shared by
/// install, replace, and doctor --fix so all three register identically.
pub fn register_argv(disk: &str) -> Vec<String> {
    vec![
        "efibootmgr".into(),
        "-c".into(),
        "-g".into(),
        "-d".into(),
        format!("/dev/{disk}"),
        "-p".into(),
        "1".into(),
        "-L".into(),
        format!("debian-{disk}"),
        "-l".into(),
        SHIM_LOADER.into(),
    ]
}

/// the mkfs.msdos argv that formats an esp as fat32 (label EFI), optionally stamping
/// the shared volume id. one definition of the flags for install's format, replace's
/// rebuild, and doctor's re-stamp; the shell sites join it (passing "$u" as the
/// volid, expanded by the shell), the argv site uses it directly.
pub fn mkfs_esp_argv(dev: &str, volid: Option<&str>) -> Vec<String> {
    let mut a = vec![
        "mkfs.msdos".into(),
        "-F".into(),
        "32".into(),
        "-s".into(),
        "1".into(),
        "-n".into(),
        "EFI".into(),
    ];
    if let Some(v) = volid {
        a.push("-i".into());
        a.push(v.into());
    }
    a.push(dev.into());
    a
}

/// the debconf-set-selections input that preseeds grub-efi (shared by the install
/// step and doctor's debconf fix), one `<pkg> <key> boolean <value>` line per key.
pub fn grub_debconf_selections() -> String {
    GRUB_DEBCONF
        .iter()
        .map(|(k, v)| format!("{GRUB_PKG} {k} boolean {v}\n"))
        .collect()
}

/// the two grub-install invocations: the named EFI/debian layout and the removable
/// EFI/BOOT fallback. shared by install (in the chroot) and doctor's grub fix (on
/// the running system). both --no-nvram -- raiden owns the nvram entries itself.
pub fn grub_install_steps(chroot: bool) -> Vec<Step> {
    let named = Step::run(
        "install grub to the esp (named)",
        &[
            "grub-install",
            "--target=x86_64-efi",
            "--bootloader-id=debian",
            "--efi-directory=/boot/efi",
            "--no-nvram",
            "--recheck",
            "--no-floppy",
        ],
    );
    let removable = Step::run(
        "install grub to the esp (removable fallback)",
        &[
            "grub-install",
            "--target=x86_64-efi",
            "--efi-directory=/boot/efi",
            "--no-nvram",
            "--recheck",
            "--no-floppy",
            "--removable",
        ],
    );
    if chroot {
        vec![named.chroot(), removable.chroot()]
    } else {
        vec![named, removable]
    }
}
