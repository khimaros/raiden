#!/usr/bin/python3
"""write random byte content at random positions within a file or block device,
to simulate bitrot for raid/integrity testing. ported from raid-explorations."""

import os
import random
import sys

if len(sys.argv) < 3:
    print("usage: random_write.py <path> <bytes> [start_pad] [end_pad]")
    sys.exit(1)

of = sys.argv[1]
count = int(sys.argv[2])
start_pad = int(sys.argv[3]) if len(sys.argv) > 3 else 512 * 1024 * 1024
end_pad = int(sys.argv[4]) if len(sys.argv) > 4 else 128 * 1024 * 1024

fd = os.open(of, os.O_RDONLY)
end = os.lseek(fd, 0, os.SEEK_END)
os.close(fd)

print("[*] write %d random bytes to %s (size %d, start_pad %d, end_pad %d)"
      % (count, of, end, start_pad, end_pad))

changes = 0
while changes < count:
    seek = random.randint(start_pad, end - end_pad)
    with open(of, "rb") as f:
        f.seek(seek)
        cur = f.read(1)
    with open(of, "wb") as f:
        f.seek(seek)
        new = bytes([random.randint(0, 255)])
        if not cur[0] or cur == new:
            continue
        changes += 1
        f.write(new)
