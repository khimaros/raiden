"""unit tests for the serial-console control-sequence stripping (no libvirt)."""

import io

from raiden_e2e.log import CleanStream, Tee, strip_control


def test_strip_control_removes_escapes_and_keeps_text():
    # csi color codes, a carriage return, and a bell go away; text stays.
    s = "\x1b[0;32m[ OK \x1b[0m] started\r\x07 thing\ttab\n"
    assert strip_control(s) == "[ OK ] started thing\ttab\n"


def test_strip_control_keeps_newlines_and_tabs():
    assert strip_control("a\nb\tc") == "a\nb\tc"


def test_strip_control_removes_shell_integration_osc_marker():
    # debian's bash emits osc 3008 shell-integration markers (terminated by ST,
    # ESC backslash) around each command. they must be stripped so console.run()
    # returns clean text -- otherwise a value like a device path comes back
    # wrapped in the marker and downstream string compares break.
    s = "\x1b]3008;start=abc;cwd=/root\x1b\\/dev/vda2"
    assert strip_control(s) == "/dev/vda2"


def test_cleanstream_line_buffers_split_escape_sequence():
    buf = io.StringIO()
    cs = CleanStream(buf)
    # an escape sequence split across two writes must still be stripped.
    cs.write("hello \x1b[1")
    cs.write("2mworld\n")
    assert buf.getvalue() == "hello world\n"


def test_cleanstream_holds_partial_line_until_close():
    buf = io.StringIO()
    cs = CleanStream(buf)
    cs.write("no newline yet")
    assert buf.getvalue() == ""  # buffered, not emitted
    cs.close()
    assert buf.getvalue() == "no newline yet"


def test_tee_fans_out_to_all_streams():
    a, b = io.StringIO(), io.StringIO()
    Tee(a, b).write("x")
    assert a.getvalue() == "x" and b.getvalue() == "x"
