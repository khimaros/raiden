#!/bin/bash
# fsync-bound fileio benchmark: every write is fsync'd, so this measures durable
# write latency (the relevant metric for crash-consistent raid), not page-cache
# throughput. prints total time and 95th percentile per pass; lower is better.
# the 2g working set and per-mode event counts restore raid-explorations'
# pre-regression sizing (it had shrunk to 500m / 2000 events), so the 95th
# percentile is stable again. random writes churn more per event than sequential,
# so they need fewer events to converge.

set -e

command -v sysbench >/dev/null || apt-get install -y sysbench

SIZE=2G
PASSES=3

# run on the array (root fs), NOT /tmp: recent systemd mounts /tmp as tmpfs
# (ram), which would benchmark ram instead of the raid stack and cannot even hold
# the 2g working set. /var/tmp is on the root filesystem per the fhs.
dir=$(mktemp -d -p /var/tmp)
cd "$dir"
sysbench fileio prepare --file-total-size=$SIZE >/dev/null

run() {  # mode events
    for i in $(seq 1 "$PASSES"); do
        echo "[*] $1 pass #$i"
        sysbench fileio run --file-total-size=$SIZE --file-test-mode="$1" \
            --file-fsync-all=on --time=0 --events="$2" 2>&1 \
            | grep -E '(total time|95th percentile):' | sed -r 's/[[:space:]]+/ /g'
    done
}

run rndwr 5000
run seqwr 20000

cd /
rm -rf "$dir"
