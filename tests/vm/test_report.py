"""unit tests for the harness logic that does not need libvirt: the report
model, config derivation, and the domain xml builder. these run anywhere."""

from raiden_e2e import domain, scenarios
from raiden_e2e.config import VMConfig
from raiden_e2e.report import FAIL, PASS, Report


def test_report_flushes_partial_results_to_disk(tmp_path):
    out = tmp_path / "report.md"
    r = Report("dm-crypt~btrfs", "raid1", "2026-06-20T00:00:00", out_path=str(out))
    r.add("install", "run", PASS, "ok")
    # the file exists after the first check, before the run finishes.
    assert out.exists()
    assert "install" in out.read_text()


def test_report_grades_and_renders():
    r = Report("dm-crypt~md~lvm~ext4", "6", "2026-06-20T00:00:00")
    r.add("install", "run", PASS, "ok")
    r.add("corrupt 2/4 data", "survive", FAIL, "rc=1")
    r.mark_complete()
    md = r.to_markdown()
    assert "dm-crypt~md~lvm~ext4" in md
    assert "FAILED" in md
    assert len(r.failed()) == 1


def test_report_incomplete_until_run_finishes():
    # a report flushed before the run finishes must NOT read OK, even if nothing
    # has failed yet -- otherwise a killed/hung run looks like a clean pass.
    r = Report("dm-crypt~btrfs", "raid1c3", "2026-06-20T00:00:00")
    r.add("install", "run", PASS, "ok")
    assert "INCOMPLETE" in r.to_markdown()
    assert "OK" not in r.to_markdown()
    # only marking the run complete flips it to OK.
    r.mark_complete()
    md = r.to_markdown()
    assert "**OK**" in md
    assert "INCOMPLETE" not in md


def test_report_embeds_configuration():
    from raiden_e2e.vm import summarize_config

    rows = summarize_config(
        '[install]\nrelease = "forky"\nboot_mode = "efi"\n'
        '[disks]\nmembers = ["vda", "vdb"]\n'
        '[raid]\nstack = "dm-crypt~zfs"\nlevel = "raidz2"\n'
        '[crypt]\ncipher = "aes-xts-plain64"\nkey_size = 512\nsector_size = 4096\nintegrity = "none"\n'
        "[boot]\nraid = false\n"
    )
    r = Report("dm-crypt~zfs", "raidz2", "2026-06-20T00:00:00", config_rows=rows)
    md = r.to_markdown()
    assert "configuration:" in md
    assert "| field | value |" in md
    assert "| crypt cipher | aes-xts-plain64 |" in md
    assert "| /boot | independent |" in md
    # absent config -> no configuration section (back-compat).
    assert "configuration:" not in Report("s", "l", "t").to_markdown()


def test_config_defaults_level_and_disks():
    cfg = VMConfig(stack="dm-crypt~zfs")
    assert cfg.resolved_level() == "raidz2"
    assert cfg.disk_names() == ["vda", "vdb", "vdc", "vdd"]


def test_detect_keywords():
    assert scenarios._detected("md: read error corrected")
    assert not scenarios._detected("all clean, no issues")


def test_domain_xml_boot_order():
    cfg = VMConfig(token="abc123", iso="/srv/x.iso")
    disks = [("vda", "/srv/vda.raw"), ("vdb", "/srv/vdb.raw")]
    # default: disks lead (boot order 1,2), cdrom is the fallback (order 3).
    normal = domain.build_xml(cfg, disks=disks, nvram_path="/srv/nv.fd", payload_dir="/srv/p")
    assert "raiden-e2e-abc123" in normal
    assert 'type="raw"' in normal and "virtiofs" in normal and "serial" in normal
    # disks must use cache=none so the host-side truncate scenario is seen by the
    # guest (writeback would resurrect the zeroed pages).
    assert 'cache="none"' in normal
    assert normal.index('boot order="1"') < normal.index('device="cdrom"')

    # cdrom-first: the cdrom gets boot order 1 (forces a livecd boot for rescue).
    rescue = domain.build_xml(
        cfg, disks=disks, nvram_path="/srv/nv.fd", payload_dir="/srv/p", cdrom_first=True
    )
    cdrom_block = rescue[rescue.index('device="cdrom"'):]
    assert 'boot order="1"' in cdrom_block


def test_domain_firmware_matches_boot_mode():
    from raiden_e2e.config import EXAMPLES_DIR

    disks = [("vda", "/srv/vda.raw")]
    kw = dict(disks=disks, nvram_path="/srv/nv.fd", payload_dir="/srv/p")
    # efi -> ovmf (pflash loader + per-vm nvram).
    efi = VMConfig(token="a", iso="/x.iso")
    assert efi.resolved_boot_mode() == "efi"
    assert "pflash" in domain.build_xml(efi, **kw)
    # bios -> seabios (no ovmf loader/nvram).
    bios = VMConfig(
        token="b", iso="/x.iso",
        config_file=str(EXAMPLES_DIR / "dm-crypt~md~lvm~ext4.raid6.bios.toml"),
    )
    assert bios.resolved_boot_mode() == "bios"
    xml = domain.build_xml(bios, **kw)
    assert "pflash" not in xml and "<nvram" not in xml
