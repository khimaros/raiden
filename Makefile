.PHONY: build dist precommit test test-e2e test-vm-unit test-vm test-vm-install test-vm-boot analysis fmt clippy clean

build:
	cargo build

# fully static x86_64 binary for a live environment: no toolchain or shared libs
# needed on the target, so a bare Debian live can `wget` it and run (see
# install.sh). requires the musl target (rustup target add $(DIST_TARGET)).
DIST_TARGET := x86_64-unknown-linux-musl
dist:
	cargo build --release --target $(DIST_TARGET)
	@echo "built target/$(DIST_TARGET)/release/raiden (static; publish as raiden-x86_64-linux-musl)"

precommit: fmt clippy test build

fmt:
	cargo fmt --all

clippy:
	cargo clippy --all-targets -- -D warnings

test:
	cargo test

# fast, hermetic e2e: planning, validation, and resume via --dry-run (no vm).
test-e2e:
	cd tests && uv run pytest

# unit tests for the vm harness logic (report, config, domain xml); no vm.
test-vm-unit:
	cd tests/vm && uv run pytest

# full libvirt vm install + resilience run. requires kvm and ISO=<debian live>.
# the graded report is written to a timestamped file under tests/vm/reports/ so
# every run is kept; OUT=<path> overrides it. optional: STACK=<stack> (installs
# the matching examples/ config), CONFIG=<path> (a specific config instead),
# SCENARIO=<name[,name]> (subset; see --list-scenarios), SKIP_BENCH=1 (drop the
# costly sysbench pass; for fast correctness/troubleshooting runs), TAG=<label>
# (folded into the report filename, eg. to tell same-stack levels apart like
# raid6 vs raid10), BOOT_RAID=1 (md raid1 /boot instead of the default independent
# /boot), KEEP=1 (leave the vm + disks + console.log for inspection).
test-vm:
	cargo build --release
	cd tests/vm && uv run python -m raiden_e2e.run --iso "$(ISO)" --stack "$(or $(STACK),dm-crypt~md~lvm~ext4)" $(if $(CONFIG),--config "$(CONFIG)") $(if $(SCENARIO),--scenario "$(SCENARIO)") $(if $(SKIP_BENCH),--skip-benchmark) $(if $(TAG),--tag "$(TAG)") $(if $(BOOT_RAID),--boot-raid) $(if $(KEEP),--keep) $(if $(OUT),--out "$(OUT)")

# fast live/install check: boot the livecd, run the install (through livecd.sh, as
# on real hardware), and stop before the post-install scenarios. ISO=<debian live>;
# optional STACK=, CONFIG=, BOOT_RAID=1, KEEP=1.
test-vm-install:
	cargo build --release
	cd tests/vm && uv run python -m raiden_e2e.run --iso "$(ISO)" --install-only --stack "$(or $(STACK),dm-crypt~md~lvm~ext4)" $(if $(CONFIG),--config "$(CONFIG)") $(if $(BOOT_RAID),--boot-raid) $(if $(KEEP),--keep) --tag install $(if $(OUT),--out "$(OUT)")

# standalone boot-corruption test on a fresh install. kept out of the default run
# (bundled after the other corruptions its boot failures would be confounded by
# accumulated state), so it gets its own clean install here. ISO=<debian live>.
# optional: STACK=, CONFIG=, KEEP=1, BOOT_RAID=1 (as above).
test-vm-boot:
	cargo build --release
	cd tests/vm && uv run python -m raiden_e2e.run --iso "$(ISO)" --stack "$(or $(STACK),dm-crypt~md~lvm~ext4)" $(if $(CONFIG),--config "$(CONFIG)") $(if $(BOOT_RAID),--boot-raid) $(if $(KEEP),--keep) --scenario corrupt_efiboot --tag boot $(if $(OUT),--out "$(OUT)")

# supervised vm run: pauses for the operator on unexpected console state.
analysis:
	cargo build --release
	cd tests/vm && uv run python -m raiden_e2e.run --interactive --iso "$(ISO)" --stack "$(or $(STACK),dm-crypt~md~lvm~ext4)" $(if $(CONFIG),--config "$(CONFIG)") --tag analysis $(if $(OUT),--out "$(OUT)")

clean:
	cargo clean
