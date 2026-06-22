"""command line entrypoint: run the full vm e2e for one stack and write a report.

  python -m raiden_e2e.run --iso /path/debian-live.iso --stack dm-crypt~md~lvm~ext4

requires a libvirt/kvm host with virsh, qemu-img, xorriso, and OVMF. see
README.md. exits nonzero if any graded check failed."""

from __future__ import annotations

import argparse
import datetime
import os
import sys
from pathlib import Path

from . import scenarios as sc
from .config import EXAMPLE_CONFIG, VMConfig
from .runner import run

REPO = Path(__file__).resolve().parents[3]
# timestamped reports accumulate here so every run is kept (a subdir of tests/vm).
REPORTS_DIR = Path(__file__).resolve().parents[1] / "reports"


def default_out(stack: str, tag: str, when: datetime.datetime) -> str:
    """a timestamped report path so runs accumulate a history instead of
    overwriting one file: reports/<stack>[-<tag>]-<YYYYmmdd-HHMMSS>.md. the stack
    name keeps its ~ separators."""
    parts = [stack] + ([tag] if tag else []) + [when.strftime("%Y%m%d-%H%M%S")]
    return str(REPORTS_DIR / ("-".join(parts) + ".md"))


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(prog="raiden_e2e")
    p.add_argument("--iso", default="", help="path to a debian live iso")
    p.add_argument("--stack", default="dm-crypt~md~lvm~ext4")
    p.add_argument("--level", default="", help="raid level (default per stack)")
    p.add_argument(
        "--boot-raid",
        action="store_true",
        help="put /boot on an md raid1 array (default: independent per-disk /boot)",
    )
    p.add_argument(
        "--scenario",
        action="append",
        default=[],
        metavar="NAME",
        help="run only these post-install scenarios (repeatable or comma-separated); "
        "default runs all. see --list-scenarios",
    )
    p.add_argument(
        "--skip-benchmark",
        action="store_true",
        help="drop the sysbench benchmark (the costly fileio pass) from the run -- "
        "for fast correctness or troubleshooting runs; the resilience checks remain",
    )
    p.add_argument(
        "--list-scenarios",
        action="store_true",
        help="print the selectable scenario names and exit",
    )
    p.add_argument("--binary", default=str(REPO / "target" / "release" / "raiden"))
    p.add_argument(
        "--config",
        default="",
        metavar="PATH",
        help="raiden config to install from (default: the matching examples/ config)",
    )
    p.add_argument("--disks", type=int, default=4)
    p.add_argument("--disk-size-gb", type=int, default=5)
    p.add_argument("--memory-mb", type=int, default=4096)
    p.add_argument("--vcpus", type=int, default=4)
    p.add_argument("--image-dir", default="")
    p.add_argument("--ovmf-code", default="/usr/share/OVMF/OVMF_CODE_4M.fd")
    p.add_argument("--ovmf-vars", default="/usr/share/OVMF/OVMF_VARS_4M.fd")
    p.add_argument("--name", default="", help="fixed run name/token (reusable with --skip-install)")
    p.add_argument("--skip-install", action="store_true", help="reuse existing disks; skip install")
    p.add_argument(
        "--install-only",
        action="store_true",
        help="stop after a successful install (skip the post-install scenarios) -- "
        "a fast check of the live/install path",
    )
    p.add_argument("--interactive", action="store_true", help="pause for the operator on unexpected state")
    p.add_argument("--keep", action="store_true", help="keep the vm and disks after the run")
    p.add_argument(
        "--tag",
        default="",
        help="label folded into the default timestamped report name (eg. boot)",
    )
    p.add_argument(
        "--out",
        default="",
        help="write the report here (default: a timestamped file under reports/)",
    )
    args = p.parse_args(argv)

    if args.list_scenarios:
        print("\n".join(sc.scenario_names()))
        return 0
    if not args.iso:
        print("--iso is required (path to a debian live iso)", file=sys.stderr)
        return 2
    if not args.config and args.stack not in EXAMPLE_CONFIG:
        print(
            f"unknown stack {args.stack!r}; choose from: {', '.join(EXAMPLE_CONFIG)} "
            "or pass --config <path>",
            file=sys.stderr,
        )
        return 2

    # --scenario is repeatable and accepts comma-separated lists; validate names.
    selected = [n for item in args.scenario for n in item.split(",") if n]
    # --skip-benchmark drops sysbench: from the explicit selection if given, else
    # from the default bundle (which it materializes so rescue still runs).
    if args.skip_benchmark:
        selected = [n for n in (selected or sc.default_scenario_names()) if n != sc.BENCHMARK]
    known = sc.scenario_names()
    unknown = [n for n in selected if n not in known]
    if unknown:
        print(
            f"unknown scenario(s): {', '.join(unknown)}; choose from: {', '.join(known)}",
            file=sys.stderr,
        )
        return 2

    cfg = VMConfig(
        stack=args.stack,
        level=args.level,
        boot_raid=args.boot_raid,
        scenarios=tuple(selected),
        iso=os.path.abspath(args.iso),
        binary=os.path.abspath(args.binary),
        config_file=os.path.abspath(args.config) if args.config else "",
        disks=args.disks,
        disk_size_gb=args.disk_size_gb,
        memory_mb=args.memory_mb,
        vcpus=args.vcpus,
        image_dir=args.image_dir,
        ovmf_code=args.ovmf_code,
        ovmf_vars=args.ovmf_vars,
        interactive=args.interactive,
        keep=args.keep,
        skip_install=args.skip_install,
        install_only=args.install_only,
        token=args.name,
    )
    if args.skip_install and not args.name:
        print("--skip-install needs --name <run> to find the disks from a prior --keep run", file=sys.stderr)
        return 2
    if args.skip_install and args.install_only:
        print("--skip-install and --install-only are mutually exclusive", file=sys.stderr)
        return 2
    if not Path(cfg.iso).exists():
        print(f"iso not found at {cfg.iso} (paths resolve from the current directory)", file=sys.stderr)
        return 2
    if not Path(cfg.binary).exists():
        print(f"raiden binary not found at {cfg.binary}; build it first (cargo build --release)", file=sys.stderr)
        return 2
    if not Path(cfg.resolved_config_file()).exists():
        print(f"config not found at {cfg.resolved_config_file()}", file=sys.stderr)
        return 2

    out = args.out or default_out(cfg.stack, args.tag, datetime.datetime.now())
    Path(out).parent.mkdir(parents=True, exist_ok=True)

    # run() rewrites the report after every check, so an interrupted run still
    # leaves a partial report at out.
    report = run(cfg, out=out)
    Path(out).write_text(report.to_markdown())
    print(f"\nreport written to {out}\n")
    print(report.to_markdown())
    return 1 if report.failed() else 0


if __name__ == "__main__":
    sys.exit(main())
