#!/bin/bash
# read every file on the root filesystem and report read failures. exits nonzero
# if any file could not be read (a userspace-visible data-loss signal), so the
# harness can grade "survive" without parsing output.

set -uo pipefail

errs=$(find / -xdev -type f -exec md5sum {} \; 2>&1 >/dev/null | wc -l)
echo "survival: $errs read error(s)"
[ "$errs" -eq 0 ]
