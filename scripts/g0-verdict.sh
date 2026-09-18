#!/usr/bin/env bash
# Gate G0 (ROADMAP.md) from one soak's two CSVs, threshold by threshold,
# counting only samples after hour 1 as the gate does:
#   mean physical footprint < 150 MB, linear-fit slope <= 2 MB/h,
#   (max - min) <= 25 MB, idle CPU ~ 0 %.
# Footprint is what G0 names; RSS is printed beside it for comparison. Idle
# CPU has no number in the gate, so it is reported for the maintainer to judge.
#
# Usage: scripts/g0-verdict.sh <rss-*.csv from measure.sh> <gpu-*.csv from measure-footprint.sh>
set -euo pipefail

RSS=${1:?usage: g0-verdict.sh <rss.csv> <gpu.csv>}
GPU=${2:?usage: g0-verdict.sh <rss.csv> <gpu.csv>}

# stats <csv> <value column>: after-hour-1 count, mean, min, max, slope (MB/h), hours covered.
stats() {
  awk -F, -v col="$2" '
    NR == 2 { start = $1 }
    NR >= 2 && $1 >= start + 3600 && $col != "" {
      x = ($1 - start) / 3600; y = $col
      n++; sx += x; sy += y; sxx += x * x; sxy += x * y
      if (n == 1 || y < min) min = y
      if (n == 1 || y > max) max = y
      last = x
    }
    END {
      if (n < 2) { print "0 0 0 0 0 0"; exit }
      slope = (n * sxy - sx * sy) / (n * sxx - sx * sx)
      printf "%d %.1f %.1f %.1f %.2f %.1f\n", n, sy / n, min, max, slope, last
    }' "$1"
}

read -r fn fmean fmin fmax fslope fhours < <(stats "$GPU" 6)
read -r rn rmean rmin rmax rslope _ < <(stats "$RSS" 3)
[ "$fn" -ge 2 ] || { echo "fewer than two footprint samples after hour 1 — run longer" >&2; exit 1; }

cpu=$(awk -F, 'NR == 2 { s = $1 } NR >= 2 && $1 >= s + 3600 { n++; c += $4 } END { if (n) printf "%.2f", c / n; else print "n/a" }' "$RSS")
gpu=$(awk -F, 'NR == 2 { s = $1 } NR >= 2 && $1 >= s + 3600 && $3 != "" {
        if (!seen) { g0 = $3; t0 = $1; seen = 1 } g1 = $3; t1 = $1 }
      END { if (t1 > t0) printf "%.1f", (g1 - g0) / 1e6 / ((t1 - t0) / 60); else print "n/a" }' "$GPU")
total=$(awk -F, 'NR == 2 { s = $1 } NR >= 2 { e = $1 } END { printf "%.1f", (e - s) / 3600 }' "$GPU")

met() { awk -v v="$1" -v op="$2" -v lim="$3" 'BEGIN {
  ok = (op == "<") ? (v < lim) : (v <= lim); print ok ? "met" : "not met" }'; }
fspan=$(awk -v a="$fmax" -v b="$fmin" 'BEGIN { printf "%.1f", a - b }')
rspan=$(awk -v a="$rmax" -v b="$rmin" 'BEGIN { printf "%.1f", a - b }')

echo "Run: ${total} h in total; after hour 1: ${fn} footprint samples (${fhours} h in), ${rn} RSS samples."
echo
echo "| G0 threshold (after hour 1) | Footprint | | RSS |"
echo "|---|---|---|---|"
echo "| Mean < 150 MB | ${fmean} MB | $(met "$fmean" "<" 150) | ${rmean} MB |"
echo "| Linear-fit slope <= 2 MB/h | ${fslope} MB/h | $(met "$fslope" "<=" 2) | ${rslope} MB/h |"
echo "| (max - min) <= 25 MB | ${fspan} MB (${fmin}-${fmax}) | $(met "$fspan" "<=" 25) | ${rspan} MB |"
echo "| Idle CPU ~ 0 % | ${cpu} % of one core (ps mean) | for the maintainer | |"
echo
echo "GPU after hour 1: ${gpu} ms per minute."
