#!/usr/bin/env bash
# Physical footprint + GPU sampler for gate G0 (ROADMAP.md), run beside
# scripts/measure.sh. Every 60 s it appends one CSV row for a running PR
# Marmot: the physical footprint (`footprint -p`, Activity Monitor's Memory
# column) and the app's Metal GPU time from the IO registry. Read-only, no
# sudo, and it never touches the app's window.
#
# Columns: epoch, iso_time, gpu_ns_total (accumulatedGPUTime), last_submit_ns
# (lastSubmittedTime), mach_now_ns (CLOCK_UPTIME_RAW, the clock of
# last_submit_ns), footprint_mb.
#
# Usage: scripts/measure-footprint.sh <pid> [outdir]   (default outdir: measurements/)
# Stop:  ctrl-C, or it stops by itself when the process exits.
set -euo pipefail

PID=${1:?usage: measure-footprint.sh <pid> [outdir]}
OUT=${2:-measurements}
mkdir -p "$OUT"
CSV="$OUT/gpu-$(date +%Y%m%d-%H%M%S).csv"

echo "epoch,iso_time,gpu_ns_total,last_submit_ns,mach_now_ns,footprint_mb" > "$CSV"
echo "sampling PID $PID every 60s -> $CSV"

gpu_field() {
  ioreg -l -w0 -c AGXDeviceUserClient \
    | grep -B1 "\"IOUserClientCreator\" = \"pid $PID," \
    | grep -oE "\"$1\"=[0-9]+" | head -1 | cut -d= -f2
}

while kill -0 "$PID" 2>/dev/null; do
  # "prmarmot [123]: 64-bit    Footprint: 187 MB (16384 bytes per page)"
  footprint_mb=$(footprint -p "$PID" 2>/dev/null | awk '
    /Footprint:/ {
      for (i = 1; i < NF; i++) if ($i == "Footprint:") {
        v = $(i + 1); u = $(i + 2)
        if (u == "KB") v /= 1024; else if (u == "GB") v *= 1024; else if (u == "B") v /= 1048576
        printf "%.1f", v; exit
      }
    }') || true
  [ -z "$footprint_mb" ] && break
  now_ns=$(/usr/bin/python3 -c 'import time; print(time.clock_gettime_ns(time.CLOCK_UPTIME_RAW))')
  printf '%s,%s,%s,%s,%s,%s\n' "$(date +%s)" "$(date -u +%FT%TZ)" \
    "$(gpu_field accumulatedGPUTime)" "$(gpu_field lastSubmittedTime)" "$now_ns" "$footprint_mb" >> "$CSV"
  sleep 60
done
echo "process $PID gone; samples in $CSV"
