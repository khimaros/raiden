#!/bin/sh
# raiden live-cd driver: prepare a freshly-booted debian live environment and run
# raiden under screen, so a long install survives a dropped ssh/console session.
#
# interactive use -- download then run (do NOT pipe to sh: the script must be a
# file to re-exec under sudo, and init/screen need a terminal). it self-elevates
# with sudo and runs init then install by default:
#   wget -qO /tmp/livecd.sh https://raw.githubusercontent.com/khimaros/raiden/master/livecd.sh && sh /tmp/livecd.sh
#
# set RAIDEN_REVIEW=1 to stop after init (drop to a shell to review/edit
# raiden.toml, then run raiden install yourself).
#
# the init/install/rescue subcommands are thin wrappers around the raiden binary
# (located via RAIDEN_BIN, default: on PATH). the vm test harness stages this
# script and drives raiden through them, so the live flow and the tested flow
# share one definition of the raiden invocation.
set -eu

REPO="khimaros/raiden"
RAIDEN_BIN="${RAIDEN_BIN:-raiden}"
SCREEN_SESSION="${RAIDEN_SCREEN:-raiden}"

require_root() {
    [ "$(id -u)" = 0 ] || {
        echo "run as root, eg. sudo sh $0" >&2
        exit 1
    }
}

# print a url to stdout via curl or wget (whichever the live image ships).
_download() {
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL "$1"
    elif command -v wget >/dev/null 2>&1; then
        wget -qO- "$1"
    else
        echo "need curl or wget" >&2
        return 1
    fi
}

# raiden installs its own provisioning tools (sgdisk, mdadm, debootstrap, ...)
# during its apt phase, so the live env only needs screen added up front.
deps() {
    echo "updating apt and installing screen"
    apt-get update
    DEBIAN_FRONTEND=noninteractive apt-get install -y screen
}

# get the prebuilt static binary onto the box. reuse install.sh when it sits
# beside this script; otherwise fetch it from the repo.
fetch() {
    dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
    if [ -f "$dir/install.sh" ]; then
        sh "$dir/install.sh"
    else
        _download "https://raw.githubusercontent.com/$REPO/master/install.sh" | sh
    fi
}

ensure_bin() {
    if ! command -v "$RAIDEN_BIN" >/dev/null 2>&1; then
        echo "raiden not found; installing the prebuilt binary"
        fetch
        RAIDEN_BIN=/usr/local/bin/raiden
    fi
}

# the interactive flow: prep the env, then hand off to screen running init and a
# shell, so the operator reviews the config and runs the (long) install inside a
# session they can reattach to.
# elevate to root so the whole flow (apt, screen, install) runs as root; live
# images autologin a sudo-capable user. needs $0 to be a real file to re-exec --
# hence download then run, not a pipe. sudo -E carries RAIDEN_REVIEW/RAIDEN_SCREEN.
elevate() {
    [ "$(id -u)" = 0 ] && return 0
    if [ -r "$0" ] && command -v sudo >/dev/null 2>&1; then
        echo "elevating with sudo"
        exec sudo -E sh "$0" run
    fi
    echo "run as root: download livecd.sh and run it (do not pipe to sh)" >&2
    exit 1
}

run() {
    elevate
    deps
    ensure_bin
    export RAIDEN_BIN
    echo
    if [ -n "${RAIDEN_REVIEW:-}" ]; then
        echo "starting screen session '$SCREEN_SESSION': raiden init, then a shell."
        echo "review raiden.toml, then run: raiden install"
        inner='"$RAIDEN_BIN" init; echo "review raiden.toml, then run: raiden install"; exec "${SHELL:-/bin/sh}"'
    else
        echo "starting screen session '$SCREEN_SESSION': raiden init, then install."
        echo "you choose the options (init), then confirm the ERASE and enter the"
        echo "encryption password (install)."
        inner='"$RAIDEN_BIN" init && "$RAIDEN_BIN" install; exec "${SHELL:-/bin/sh}"'
    fi
    echo "if you disconnect, reattach with: screen -r $SCREEN_SESSION"
    echo
    exec screen -S "$SCREEN_SESSION" sh -c "$inner"
}

cmd_init() { exec "$RAIDEN_BIN" init "$@"; }
cmd_install() { exec "$RAIDEN_BIN" install --verbose "$@"; }
cmd_rescue() { exec "$RAIDEN_BIN" rescue --verbose "$@"; }

sub=${1:-run}
[ $# -gt 0 ] && shift
case "$sub" in
    run) run "$@" ;;
    deps) require_root; deps ;;
    fetch) fetch ;;
    init) cmd_init "$@" ;;
    install) cmd_install "$@" ;;
    rescue) cmd_rescue "$@" ;;
    *)
        echo "usage: $0 [run|deps|fetch|init|install|rescue]" >&2
        exit 2
        ;;
esac
