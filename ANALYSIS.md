# analysis: stack comparison

a side-by-side reading of every vm e2e run on debian forky. each run is a real
install over a serial console, the corruption/fault + repair scenarios, and a
livecd rescue; the seven full runs also include the fsync-bound sysbench
benchmark, while three boot-variation runs skip the benchmark to isolate a boot
path. the per-run reports and their full run logs are in
[tests/vm/reports/](tests/vm/reports/). all ten passed (0 failing checks); this
document summarizes and compares what they revealed.

## what was tested

ten runs across the stacks, raid levels, and boot paths. the first seven are
benchmarked; the last three skip the benchmark to exercise a specific boot
variation.

| stack | raid | crypt cipher | data integrity source | boot | benchmark |
| --- | --- | --- | --- | --- | --- |
| dm-crypt~zfs | raidz2 | aes-xts (plain) | zfs checksums | efi, independent | yes |
| dm-crypt~md~lvm~ext4 | raid10 | aegis128 (aead) | dm-crypt aead | efi, independent | yes |
| dm-crypt~md~lvm~ext4 | raid6 | aegis128 (aead) | dm-crypt aead | efi, independent | yes |
| dm-crypt~md~lvm~xfs | raid10 | aegis128 (aead) | dm-crypt aead | efi, independent | yes |
| dm-crypt~md~lvm~xfs | raid6 | aegis128 (aead) | dm-crypt aead | efi, independent | yes |
| dm-crypt~btrfs | raid1c3 | aes-xts (plain) | btrfs checksums | efi, independent | yes |
| dm-integrity~md~dm-crypt~lvm~ext4 | raid6 | aes-xts (plain) | dm-integrity (crc32c) | efi, independent | yes |
| dm-crypt~md~lvm~ext4 | raid6 | aegis128 (aead) | dm-crypt aead | bios, independent | skipped |
| dm-crypt~md~lvm~ext4 | raid6 | aegis128 (aead) | dm-crypt aead | efi, md raid1 /boot | skipped |
| dm-crypt~md~lvm~ext4 | raid6 | aegis128 (aead) | dm-crypt aead | efi, independent (first-disk esp loss) | skipped |

all: 4 members, 4096-byte crypt sectors, forky. these are the recommended
per-stack defaults, not a controlled matrix: the crypt integrity choice (aead vs
plain) and the redundancy profile both vary with the stack. read the performance
numbers as "how each recommended stack behaves," not "raid level x vs y, all else
equal" -- except where the same stack appears at two raid levels (ext4 and xfs at
both raid6 and raid10), which does isolate the level.

## workload

sysbench fileio, 2g working set, every write fsync'd (`--file-fsync-all=on`) so
the number is durable write latency -- the metric that matters for crash-
consistent raid, not page-cache throughput. lower is better. rndwr = 5000 random-
write events; seqwr = 20000 sequential-write events, three passes.

## performance

the seven benchmarked runs, sorted by sequential-write total (fastest first).

| stack (raid) | crypt | rndwr total | rndwr p95 | seqwr total (avg of 3) | seqwr p95 |
| --- | --- | --- | --- | --- | --- |
| zfs (raidz2) | plain | 56s | 15ms | 198s | ~15ms |
| xfs (raid10) | aead | 93s | 30ms | 374s | ~30ms |
| ext4 (raid10) | aead | 102s | 30ms | 406s | ~30ms |
| btrfs (raid1c3) | plain | 99s | 29ms | 463s | ~31ms |
| ext4 (raid6) | aead | 142s | 37ms | 552s | ~36ms |
| xfs (raid6) | aead | 150s | 37ms | 578s | ~36ms |
| dm-integrity ext4 (raid6) | plain + integrity | 153s | 40ms | 614s | ~38ms |

what the numbers say:

- zfs raidz2 is ~2x faster than anything else on fsync'd writes (p95 ~15ms vs
  ~30-40ms). its zil + transaction-group batching coalesces sync writes well, and
  it runs plain aes-xts (no aead). some of the edge is zfs sync semantics, so
  treat it as a real but slightly flattered lead.
- raid level dominates within a filesystem. raid10 beats raid6 by ~35% on seqwr
  for both ext4 (406s vs 552s) and xfs (374s vs 578s): a mirror has no parity
  read-modify-write on each write, while raid6 pays a double-parity rmw.
- filesystem choice is secondary on this workload. at the same raid level xfs ==
  ext4 within noise (raid10: 374s vs 406s; raid6: 578s vs 552s). the aead crypt
  and the array geometry dominate the fsync benchmark, so ext4 vs xfs barely
  moves it for single-threaded sync writes.
- btrfs raid1c3 is mid-pack despite plain crypt: 3-way mirroring plus copy-on-
  write triples the write work, which outweighs having no aead. aead is not the
  only thing that costs.
- dm-integrity is the slowest (614s, p95 ~38ms) even though it runs plain aes-
  xts. the dm-integrity layer below md journals a checksum tag with every block,
  adding the most write amplification of any stack here -- it buys block-level
  integrity without aead, at the cost of throughput and the longest install.
- aead tax in context: the two plain-crypt md-comparable points are not directly
  paired here, but btrfs (no aead) still trails both raid10 stacks (with aead)
  because 3x mirroring outweighs the aead cost. aead is real but not the dominant
  term.

## resilience

every benchmarked stack detected and repaired silent corruption, survived a
2-of-4 member fault, replaced + scrubbed clean, and rescued from a livecd. the
per-run reboot and rescue grades:

| run | reached login | degraded boot (faulty member) | livecd rescue |
| --- | --- | --- | --- |
| zfs (raidz2) | PASS | PASS, unattended | PASS (exit=0) |
| ext4 (raid10) | PASS | PASS, unattended | PASS (exit=0) |
| ext4 (raid6) | PASS | PASS, unattended | PASS (exit=0) |
| xfs (raid10) | PASS | PASS, unattended | PASS (exit=0) |
| xfs (raid6) | PASS | PASS, unattended | PASS (exit=0) |
| dm-integrity ext4 (raid6) | PASS | PASS, unattended | PASS (exit=0) |
| btrfs (raid1c3) | PASS | WARN, manual `mount -o degraded` at initramfs | WARN (exit=1, fs still mounts/reads) |
| ext4 (raid6) bios | PASS | PASS, unattended | PASS (exit=0) |
| ext4 (raid6) md raid1 /boot | PASS | PASS, unattended | WARN (exit=1) |
| ext4 (raid6) first-disk esp loss | PASS (booted from a surviving esp) | n/a (boot-path test) | n/a |

what the grades mean:

- the one operational wart is btrfs: a btrfs root with a faulty member does not
  boot unattended. it drops to the initramfs and needs a manual `mount -o
  degraded` before the system comes up (graded WARN). md assembles and zfs imports
  a degraded array automatically, so every md/zfs/dm-integrity run boots headless
  after a disk loss. the harness follows the btrfs boot through by running the
  recovery command itself, which is why it still completes -- but a real headless
  box would hang at the prompt.
- silent-corruption detection differs by stack but all caught it: btrfs and zfs by
  their own checksums, dm-integrity by its crc32c tag, the aead md stacks by the
  crypt layer's authentication plus an md scrub.
- the 4-of-4-corruption livecd rescue is clean (exit=0) for every md root with an
  independent /boot; btrfs rescue is partial (exit=1, the fs still mounts and
  reads); the md raid1 /boot variant also grades the rescue WARN (exit=1) because
  re-assembling the boot array from the livecd is the fragile path the independent
  /boot default was introduced to avoid.
- the boot-variation runs confirm the boot paths independent of filesystem: bios
  (seabios) installs and boots; first-disk esp loss still boots from a surviving
  disk's esp with independent /boot; the legacy md raid1 /boot still works but
  carries the heavier rescue.

## recommendations

- unattended / headless servers where a disk can fail and the box must still come
  up on its own: md~lvm~ext4 (or xfs), zfs, or the dm-integrity stack. do not use
  btrfs as the root unless someone can intervene at the console on a degraded
  boot.
- best durable-write performance: zfs raidz2 (~2x the nearest alternative). the
  cost is an out-of-tree module -- the install builds zfs-dkms against the running
  and target kernels (visible in the zfs run log), which adds install time and a
  toolchain dependency -- plus the cddl/gpl licensing question.
- maximum portability, in-tree only: md~lvm~ext4 or md~lvm~xfs (they perform the
  same here, so choose on filesystem features). raid10 for write speed, raid6 for
  space efficiency (survives any two disks at a ~35% heavier write cost). the aead
  default buys crypt-layer integrity; drop it (plain aes-xts) for a faster root if
  crypt-layer integrity is not required.
- block-level integrity without aead and without an out-of-tree fs:
  dm-integrity~md~dm-crypt~lvm~ext4. it is the slowest stack and has the longest
  install, but it gives a checksum on every block under a plain-cipher crypt
  layer.
- choose btrfs raid1c3 for btrfs's own features (snapshots, send/receive, flexible
  profiles) and fs-level self-healing, accepting the manual degraded-boot step and
  the 3x mirror write cost.

quick guide: headless + fast + can run dkms -> zfs. headless + in-tree only ->
md~lvm~ext4 / md~lvm~xfs (raid10 fast / raid6 dense). want per-block integrity
without aead -> dm-integrity stack. need btrfs features and have console access ->
btrfs.

## caveats

- single kvm host, 4x 5g raw images on one backing device. absolute numbers are
  not production figures; the value is the relative comparison under identical
  conditions.
- the benchmark sizing is tuned for a stable p95 in this vm, not to model a real
  workload. zfs arc/zil and zfs sync semantics may flatter its fsync numbers
  relative to the others.
- redundancy differs: raid1c3 is 3 copies, raidz2/raid6 are double parity, raid10
  is a 2-copy stripe. they do not protect equally, so a pure speed ranking
  understates what raid6/raidz2 buy in fault tolerance.
- the three boot-variation runs skip the benchmark on purpose; their value is the
  boot/rescue path, not a performance point.

## dm-crypt~bcachefs: implemented but not installable on forky

the stack is implemented (per-disk dm-crypt + multi-device bcachefs, redundancy by
`--replicas`; adds the apt.bcachefs.org repo for the out-of-tree dkms module) and
its plan validates, but it cannot be installed on debian forky right now: the
repo's `bcachefs-tools` (every suite) depends on `libsodium23`, while forky ships
`libsodium26` and does not package bcachefs-tools natively, so the tools are
uninstallable. this is an upstream/library-transition skew (the repo lags
testing), not a raiden defect. revisit when the repo rebuilds against forky's
libs, or test on a release where they match. the stack code + example remain.

## remaining gaps

- ext4/xfs without aead (plain aes-xts): would isolate the aead write tax, but it
  has no integrity layer, so the corruption-detection check cannot pass -- run it
  as a perf-only measurement, not a graded scenario.
- more raid levels for breadth: md raid1/raid5; btrfs raid1/raid10 (and raid5/6,
  which carry a known write-hole caveat worth documenting); zfs raidz1/raidz3.
- larger member counts and 512-byte sectors, if those geometries are targets.
