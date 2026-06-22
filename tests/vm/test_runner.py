"""guard the live-phase command construction. the install/rescue commands route
the raiden invocation through the staged livecd.sh (the same entrypoint a human
uses on real hardware), keeping the vm glue -- mount, tee, result file, poweroff
-- here. these assertions pin the contract so the indirection cannot silently
drop a flag or the result/poweroff handling (the full path is validated by a real
`make test-vm` run)."""


def test_install_command_drives_raiden_through_livecd():
    from raiden_e2e.runner import _INSTALL

    assert "mount -t virtiofs payload /srv/raiden" in _INSTALL
    assert "RAIDEN_BIN=/srv/raiden/raiden sh /srv/raiden/livecd.sh install" in _INSTALL
    assert (
        "--yes --config /srv/raiden/raiden.toml --password-file /srv/raiden/password"
        in _INSTALL
    )
    assert "echo $? > /srv/raiden/install.result" in _INSTALL
    assert _INSTALL.rstrip().endswith("poweroff'")


def test_rescue_command_drives_raiden_through_livecd():
    from raiden_e2e.runner import _RESCUE

    assert "RAIDEN_BIN=/srv/raiden/raiden sh /srv/raiden/livecd.sh rescue" in _RESCUE
    assert "echo $? > /srv/raiden/rescue.result" in _RESCUE
    assert "ls -A /mnt > /srv/raiden/rescue.files" in _RESCUE
    assert _RESCUE.rstrip().endswith("poweroff'")
