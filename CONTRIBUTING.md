# contributing

## build

```
make
```

this runs `cargo build`. the toolchain is pinned in `mise.toml`; with mise
installed, `mise install` provisions rust and the python used by the e2e tests.

`make dist` builds the fully static binary shipped to live environments (the
asset `install.sh` downloads). it needs the musl target once:
`rustup target add x86_64-unknown-linux-musl`.

the two live-environment entrypoints are `install.sh` (download the static
binary) and `livecd.sh` (the guided driver: install screen, fetch raiden, run
init under a screen session). the vm harness stages `livecd.sh` and drives raiden
through its `install`/`rescue` subcommands, so the live flow is what the tests
exercise -- a real `make test-vm` run validates the full path end to end.

## releases

tag `vX.Y.Z` (matching the `Cargo.toml` version) and push it, or publish a release
from the github ui. the release workflow (`.github/workflows/release.yml`) then
runs `make dist` and attaches `raiden-x86_64-linux-musl` (plus its `.sha256`) to
that release. cutting the tag is a maintainer action; CI never mutates version
control.

## before committing

```
make precommit
```

this runs formatting, clippy, the unit tests, and a build. fix everything it
reports before committing. never mutate version control on the user's behalf.

## testing

prefer end-to-end integration tests over unit tests. tests are written in python
(managed with uv) and live under `tests/`:

```
make test-e2e        # fast, hermetic: planning, validation, resume (no vm)
make test-vm-unit    # vm harness logic: report, config, domain xml (no vm)
make test-vm ISO=/path/to/debian-live.iso   # full libvirt vm install + resilience
```

`make test-e2e` exercises config parsing, validation, layout derivation, the full
command plan, and resume via `raiden config validate` / `raiden install
--dry-run`, without touching any disk. `make test-vm` is the real install
validation: it drives a libvirt/kvm vm over the serial console, fully automated,
and grades a report (see tests/vm/README.md). a few rust unit tests cover the
pure logic (config, layout, resume cursor) via `make test`.

## style

- ASCII only, everywhere.
- documentation, comments, and command line output are lowercase; CAPS only for
  acronyms or emphasis.
- comments explain "why", not "what". no changelog comments. remove dead code
  rather than commenting it out.
- keep functions under 50 lines; prefer pure functions without side effects.
- define magic constants as named values at the top of the file that uses them,
  or in a shared module when used widely.
- keep it DRY; extend existing components before adding new ones.

## dependencies

keep external dependencies to a minimum and prefer the standard library. raiden
orchestrates system binaries via std::process rather than wrapping them in
crates. propose new dependencies before adding them.

## workflow

- add a task to ROADMAP.md before starting it; mark it done when complete.
- update DESIGN.md after architectural changes and README.md after user-visible
  changes.
- for bug fixes: investigate, form a hypothesis, write a failing test, then ask
  for confirmation before fixing.
