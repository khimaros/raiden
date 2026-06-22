"""build the libvirt domain xml for a transient test vm.

mirrors the raid-explorations setup: raw member disks plus the live iso as a
cdrom, using a per-device boot order (disks first, cdrom last). with blank disks
the firmware falls through to the cdrom and boots the installer; once raiden has
installed a bootloader the firmware boots the disks. one domain, no kernel
extraction, no boot-mode switching.

the live/install phase is driven with virsh send-key over the graphical console
(the stock live iso has no serial console); the installed system enables a serial
console (raiden's serial_console option), which the harness uses thereafter."""

from __future__ import annotations

import html

from .config import VMConfig


# cache="none" (O_DIRECT) is required for the truncate-disk scenario: the harness
# zeroes a disk's backing file on the host while the guest runs, and the default
# (writeback) cache would let qemu serve -- and write back -- the stale pages,
# so the guest never actually loses the disk (md re-assembles the old members).
def _disk(dev: str, path: str, order: int) -> str:
    return f"""    <disk type="file" device="disk">
      <driver name="qemu" type="raw" cache="none"/>
      <source file="{html.escape(path)}"/>
      <target dev="{dev}" bus="virtio"/>
      <boot order="{order}"/>
    </disk>"""


def build_xml(
    cfg: VMConfig,
    *,
    disks: list[tuple[str, str]],
    nvram_path: str,
    payload_dir: str,
    cdrom_first: bool = False,
) -> str:
    # cdrom_first forces a livecd boot (for rescue) even when the disks are
    # bootable; otherwise disks lead and the cdrom is the blank-disk fallback.
    offset = 1 if cdrom_first else 0
    disk_xml = "\n".join(_disk(dev, path, i + 1 + offset) for i, (dev, path) in enumerate(disks))
    cdrom_order = 1 if cdrom_first else len(disks) + 1
    # efi boots via ovmf (a pflash loader + per-vm nvram); bios uses the built-in
    # seabios (no loader/nvram), matching what raiden installs for each boot mode.
    if cfg.resolved_boot_mode() == "bios":
        os_block = """  <os>
    <type arch="x86_64" machine="q35">hvm</type>
    <bootmenu enable="yes"/>
  </os>"""
    else:
        os_block = f"""  <os>
    <type arch="x86_64" machine="q35">hvm</type>
    <loader readonly="yes" type="pflash">{html.escape(cfg.ovmf_code)}</loader>
    <nvram template="{html.escape(cfg.ovmf_vars)}">{html.escape(nvram_path)}</nvram>
    <bootmenu enable="yes"/>
  </os>"""
    return f"""<domain type="kvm">
  <name>{cfg.name}</name>
  <memory unit="MiB">{cfg.memory_mb}</memory>
  <memoryBacking>
    <source type="memfd"/>
    <access mode="shared"/>
  </memoryBacking>
  <vcpu>{cfg.vcpus}</vcpu>
{os_block}
  <features>
    <acpi/>
    <apic/>
  </features>
  <cpu mode="host-passthrough"/>
  <on_poweroff>destroy</on_poweroff>
  <on_reboot>restart</on_reboot>
  <on_crash>destroy</on_crash>
  <devices>
    <emulator>/usr/bin/qemu-system-x86_64</emulator>
{disk_xml}
    <disk type="file" device="cdrom">
      <driver name="qemu" type="raw"/>
      <source file="{html.escape(cfg.iso)}"/>
      <target dev="sda" bus="sata"/>
      <readonly/>
      <boot order="{cdrom_order}"/>
    </disk>
    <filesystem type="mount" accessmode="passthrough">
      <driver type="virtiofs"/>
      <source dir="{html.escape(payload_dir)}"/>
      <target dir="payload"/>
    </filesystem>
    <interface type="user">
      <model type="virtio"/>
    </interface>
    <serial type="pty">
      <target type="isa-serial" port="0">
        <model name="isa-serial"/>
      </target>
    </serial>
    <console type="pty">
      <target type="serial" port="0"/>
    </console>
    <input type="keyboard" bus="usb"/>
    <input type="tablet" bus="usb"/>
    <graphics type="spice" autoport="yes">
      <listen type="address"/>
    </graphics>
    <video>
      <model type="virtio" heads="1"/>
    </video>
    <rng model="virtio">
      <backend model="random">/dev/urandom</backend>
    </rng>
  </devices>
</domain>
"""
