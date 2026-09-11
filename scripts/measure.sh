#!/usr/bin/env bash
# Overnight memory sampler for the Step-0 gate (HANDOFF §5, r2 build doc §5.1).
# Samples RSS + CPU% of a running PR Marmot every 60s into a CSV.
#
# Usage: scripts/measure.sh <pid> [outdir]     (default outdir: measurements/)
# Stop:  ctrl-C, or it stops by itself when the process exits.
set -euo pipefail

PID=${1:?usage: measure.sh <pid> [outdir]}
OUT=${2:-measurements}
mkdir -p "$OUT"
CSV="$OUT/rss-$(date +%Y%m%d-%H%M%S).csv"

echo "epoch,iso_time,rss_mb,cpu_pct" > "$CSV"
echo "sampling PID $PID every 60s -> $CSV"

while kill -0 "$PID" 2>/dev/null; do
  line=$(ps -o rss=,pcpu= -p "$PID" | tr -s ' ') || break
  [ -z "$line" ] && break
  echo "$line" | awk -v e="$(date +%s)" -v t="$(date -u +%FT%TZ)" \
    '{printf "%s,%s,%.1f,%s\n", e, t, $1/1024, $2}' >> "$CSV"
  sleep 60
done
echo "process $PID gone; samples in $CSV"
