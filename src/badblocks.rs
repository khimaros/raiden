// map raid read errors reported by md to the files they affect. the integrity
// layer (dm-integrity or dm-crypt aead) sits below md and only knows sectors,
// while ext4 sits above md and never sees the error while redundancy holds. md
// bridges the two: it logs the failing member device and sector, which we
// reverse-map through the raid stripe and the linear dmsetup offsets to ext4
// blocks, then to paths via debugfs. ported from badblocks.sh; keys off generic
// md geometry, so it works for both md stacks.

use std::path::Path;
use std::process::Command;

use regex::Regex;

const FS_BLOCK_BYTES: i64 = 4096;
const SECTOR_BYTES: i64 = 512;
const ROOT_MD_DEVICE: &str = "/dev/md/root";
const ROOT_LV_DEVICE: &str = "/dev/vg0/root";

struct Geometry {
    chunk_sectors: i64,
    data_disks: i64,
}

/// per-volume linear mapping from the ext4 lv down to the md array.
struct Affine {
    offset: i64,
    lv_sectors: i64,
}

/// print the read-error report. read-only and best-effort; absence of an array
/// or of kernel messages is normal and prints accordingly.
pub fn report() {
    if !Path::new(ROOT_MD_DEVICE).exists() {
        return;
    }
    let Some(mdbase) = md_basename() else { return };
    let mdsys = format!("/sys/block/{mdbase}/md");
    let Some(geom) = md_geometry(&mdsys) else {
        return;
    };

    println!("integrity layer messages:");
    let integ: Vec<String> = dmesg()
        .lines()
        .filter(|l| l.contains("device-mapper: integrity:") || l.contains("device-mapper: crypt:"))
        .map(|l| format!("  {l}"))
        .collect();
    if integ.is_empty() {
        println!("  none");
    } else {
        integ.iter().for_each(|l| println!("{l}"));
    }

    println!("\nraid read errors mapped to files:");
    let affine = lv_affine(&mdbase);
    let events = read_error_events();
    if events.is_empty() {
        println!("  none recorded in the kernel ring buffer");
        return;
    }
    let mut any_uncorrected = false;
    for ev in &events {
        if ev.uncorrected {
            any_uncorrected = true;
            println!(
                "  member {} sector {}: unrecoverable -- restore from backup",
                ev.dev, ev.sector
            );
            // only uncorrectable errors lost data, so only these are mapped to
            // files; corrected errors were repaired from redundancy.
            if let Some(a) = &affine {
                report_event(&mdsys, &geom, a, ev.sector, &ev.dev);
            }
        } else {
            println!(
                "  member {} sector {} ({} sectors): repaired by md from redundancy -- data intact",
                ev.dev, ev.sector, ev.nsectors
            );
        }
    }
    if any_uncorrected && affine.is_none() {
        println!(
            "  (file mapping unavailable: {ROOT_LV_DEVICE} is not a single-segment lvm volume)"
        );
    }
}

struct Event {
    uncorrected: bool,
    nsectors: i64,
    sector: i64,
    dev: String,
}

fn read_error_events() -> Vec<Event> {
    parse_read_errors(&dmesg())
}

/// parse md read-error events out of kernel ring-buffer text (pure, for tests).
/// the kernel logs two shapes: correctable as "(N sectors at S on dev)" and
/// uncorrectable ("not correctable" / "NOT corrected!!") as "(sector S on dev)".
fn parse_read_errors(text: &str) -> Vec<Event> {
    let re = Regex::new(
        r"md/raid[0-9]*:[^:]+: read error (NOT corrected|not correctable|corrected)[^(]*\((?:([0-9]+) sectors at )?(?:sector )?([0-9]+) on ([a-z0-9-]+)\)",
    )
    .unwrap();
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for line in text.lines() {
        if let Some(c) = re.captures(line) {
            let uncorrected = &c[1] != "corrected";
            let sector: i64 = c[3].parse().unwrap_or(0);
            let dev = c[4].to_string();
            // the ring buffer repeats the same error many times; dedup by event.
            if seen.insert((uncorrected, sector, dev.clone())) {
                out.push(Event {
                    uncorrected,
                    nsectors: c.get(2).and_then(|m| m.as_str().parse().ok()).unwrap_or(1),
                    sector,
                    dev,
                });
            }
        }
    }
    out
}

/// print the files sharing the raid stripe that an error landed in.
fn report_event(mdsys: &str, geom: &Geometry, a: &Affine, sector: i64, dev: &str) {
    if geom.chunk_sectors == 0 || geom.data_disks == 0 {
        return;
    }
    let doff = member_data_offset(mdsys, dev);
    let shsec = (sector - doff).max(0);
    let stripe = shsec / geom.chunk_sectors;
    let alo = stripe * geom.data_disks * geom.chunk_sectors;
    let ahi = alo + geom.data_disks * geom.chunk_sectors;
    let blocks = array_range_to_blocks(alo, ahi, a);
    let paths = blocks_to_paths(&blocks);
    if paths.is_empty() {
        println!("      (no file in this stripe -- filesystem metadata or free space)");
    } else {
        paths.iter().for_each(|p| println!("      {p}"));
    }
}

fn md_geometry(mdsys: &str) -> Option<Geometry> {
    let level = read_trim(&format!("{mdsys}/level"))?;
    let raid_disks: i64 = read_trim(&format!("{mdsys}/raid_disks"))?.parse().ok()?;
    let chunk: i64 = read_trim(&format!("{mdsys}/chunk_size"))?.parse().ok()?;
    let mut chunk_sectors = chunk / SECTOR_BYTES;
    let data_disks = match level.as_str() {
        "raid0" => raid_disks,
        "raid5" => raid_disks - 1,
        "raid6" => raid_disks - 2,
        "raid10" => raid_disks / 2,
        "raid1" => {
            chunk_sectors = 0;
            1
        }
        _ => 0,
    };
    Some(Geometry {
        chunk_sectors,
        data_disks,
    })
}

fn member_data_offset(mdsys: &str, dev: &str) -> i64 {
    if let Some(v) = read_trim(&format!("{mdsys}/dev-{dev}/offset")) {
        return v.parse().unwrap_or(0);
    }
    // fall back to any member's offset; uniform across members in practice.
    if let Ok(entries) = std::fs::read_dir(mdsys) {
        for e in entries.flatten() {
            let name = e.file_name();
            if name.to_string_lossy().starts_with("dev-") {
                if let Some(v) = read_trim(&format!("{}/offset", e.path().display())) {
                    return v.parse().unwrap_or(0);
                }
            }
        }
    }
    0
}

/// walk the dmsetup chain from the ext4 lv down to the md device, summing the
/// linear/crypt offsets. only single-segment volumes (as lvcreate produces) are
/// supported.
fn lv_affine(mdbase: &str) -> Option<Affine> {
    let md_majmin = read_trim(&format!("/sys/block/{mdbase}/dev"))?;
    let mut offset = 0i64;
    let mut lv_sectors = 0i64;
    let mut dm = cmd(&[
        "dmsetup",
        "info",
        "-c",
        "--noheadings",
        "-o",
        "name",
        ROOT_LV_DEVICE,
    ])?
    .trim()
    .to_string();
    loop {
        let table = cmd(&["dmsetup", "table", &dm])?;
        if table.lines().count() != 1 {
            return None;
        }
        let f: Vec<&str> = table.split_whitespace().collect();
        // logstart len type rest...
        if f.len() < 3 || f[0] != "0" {
            return None;
        }
        let (under, underoff) = match f[2] {
            "linear" => (f.get(3)?, f.get(4)?),
            "crypt" => (f.get(6)?, f.get(7)?),
            _ => return None,
        };
        if lv_sectors == 0 {
            lv_sectors = f[1].parse().ok()?;
        }
        offset += underoff.parse::<i64>().ok()?;
        if *under == md_majmin {
            return Some(Affine { offset, lv_sectors });
        }
        dm = read_trim(&format!("/sys/dev/block/{under}/dm/name"))?;
    }
}

/// ext4 block numbers covering an array sector range, clamped to the lv.
fn array_range_to_blocks(lo: i64, hi: i64, a: &Affine) -> Vec<i64> {
    let lo = (lo - a.offset).max(0);
    let hi = (hi - 1 - a.offset).min(a.lv_sectors - 1);
    if lo > hi {
        return Vec::new();
    }
    let first = lo * SECTOR_BYTES / FS_BLOCK_BYTES;
    let last = hi * SECTOR_BYTES / FS_BLOCK_BYTES;
    (first..=last).collect()
}

/// resolve ext4 block numbers to unique file paths via debugfs.
fn blocks_to_paths(blocks: &[i64]) -> Vec<String> {
    if blocks.is_empty() {
        return Vec::new();
    }
    let list = blocks
        .iter()
        .map(|b| b.to_string())
        .collect::<Vec<_>>()
        .join(" ");
    let icheck =
        cmd(&["debugfs", "-R", &format!("icheck {list}"), ROOT_LV_DEVICE]).unwrap_or_default();
    let mut inodes = std::collections::BTreeSet::new();
    for line in icheck.lines().skip(1) {
        if let Some(col2) = line.split_whitespace().nth(1) {
            if col2.chars().all(|c| c.is_ascii_digit()) && !col2.is_empty() {
                inodes.insert(col2.to_string());
            }
        }
    }
    if inodes.is_empty() {
        return Vec::new();
    }
    let inode_list = inodes.into_iter().collect::<Vec<_>>().join(" ");
    let ncheck = cmd(&[
        "debugfs",
        "-R",
        &format!("ncheck {inode_list}"),
        ROOT_LV_DEVICE,
    ])
    .unwrap_or_default();
    let mut paths = std::collections::BTreeSet::new();
    for line in ncheck.lines().skip(1) {
        let mut it = line.split_whitespace();
        match it.next() {
            Some(first) if first.chars().all(|c| c.is_ascii_digit()) && !first.is_empty() => {
                let path = it.collect::<Vec<_>>().join(" ");
                if !path.is_empty() {
                    paths.insert(path);
                }
            }
            _ => {}
        }
    }
    paths.into_iter().collect()
}

fn md_basename() -> Option<String> {
    std::fs::canonicalize(ROOT_MD_DEVICE)
        .ok()?
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
}

fn read_trim(path: &str) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
}

fn dmesg() -> String {
    cmd(&["dmesg"]).unwrap_or_default()
}

fn cmd(argv: &[&str]) -> Option<String> {
    let out = Command::new(argv[0]).args(&argv[1..]).output().ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // the kernel logs correctable errors as "(N sectors at S on dev)".
    #[test]
    fn parses_correctable_error() {
        let dm = "[ 99.9] md/raid:md127: read error corrected (8 sectors at 12345 on dm-3)";
        let evs = parse_read_errors(dm);
        assert_eq!(evs.len(), 1);
        assert!(!evs[0].uncorrected);
        assert_eq!(evs[0].sector, 12345);
        assert_eq!(evs[0].dev, "dm-3");
    }

    // raid5/raid6 log the uncorrectable cases as "(sector S on dev)" -- no count.
    // these are the events that actually lost data and whose files matter.
    #[test]
    fn parses_uncorrectable_not_corrected() {
        let dm = "[ 99.9] md/raid:md127: read error NOT corrected!! (sector 12345 on dm-3).";
        let evs = parse_read_errors(dm);
        assert_eq!(evs.len(), 1, "uncorrectable message must be recognized");
        assert!(evs[0].uncorrected);
        assert_eq!(evs[0].sector, 12345);
        assert_eq!(evs[0].dev, "dm-3");
    }

    #[test]
    fn parses_uncorrectable_not_correctable() {
        let dm = "[ 99.9] md/raid:md127: read error not correctable (sector 999 on dm-4).";
        let evs = parse_read_errors(dm);
        assert_eq!(evs.len(), 1);
        assert!(evs[0].uncorrected);
        assert_eq!(evs[0].sector, 999);
        assert_eq!(evs[0].dev, "dm-4");
    }

    // real ring-buffer excerpt: the same two errors repeat hundreds of times and
    // must collapse to one event each, both flagged uncorrectable.
    #[test]
    fn dedups_repeated_real_world_errors() {
        let dm = "\
[  388.898430] md/raid:md126: read error not correctable (sector 2264064 on dm-1).
[  388.899777] md/raid:md126: read error not correctable (sector 2375328 on dm-1).
[  388.901054] md/raid:md126: read error not correctable (sector 2264064 on dm-1).
[  393.900851] md/raid:md126: read error not correctable (sector 2375328 on dm-1).";
        let evs = parse_read_errors(dm);
        assert_eq!(evs.len(), 2);
        assert!(evs.iter().all(|e| e.uncorrected && e.dev == "dm-1"));
        let mut sectors: Vec<i64> = evs.iter().map(|e| e.sector).collect();
        sectors.sort();
        assert_eq!(sectors, [2264064, 2375328]);
    }

    // a corrected error and an uncorrectable one in the same buffer.
    #[test]
    fn mixed_buffer_flags_each_kind() {
        let dm = "\
md/raid:md0: read error corrected (8 sectors at 100 on dm-2)
md/raid:md0: read error NOT corrected!! (sector 200 on dm-3).";
        let evs = parse_read_errors(dm);
        assert_eq!(evs.len(), 2);
        let corrected = evs.iter().find(|e| !e.uncorrected).unwrap();
        assert_eq!((corrected.sector, corrected.nsectors), (100, 8));
        let bad = evs.iter().find(|e| e.uncorrected).unwrap();
        assert_eq!(bad.sector, 200);
    }
}
