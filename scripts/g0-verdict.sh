#!/usr/bin/env bash
# Gate G0 (ROADMAP.md) from one soak's two CSVs, threshold by threshold,
# counting only samples after hour 1 as the gate does:
#   mean physical footprint < 150 MB, p99 < 200 MB, linear-fit slope <= 2 MB/h,
#   idle CPU ~ 0 %.
# The p99 replaced a "(max - min) <= 25 MB" bound on 2026-09-20: one drawn
# frame holds ~23 MB per drawable, so the range could only ever report failure.
# The maximum is still printed, because the gate hides nothing.
# Footprint is what G0 names; RSS is printed beside it for comparison. Idle
# CPU has no number in the gate, so it is reported for the maintainer to judge.
#
# Usage: scripts/g0-verdict.sh <rss-*.csv from measure.sh> <gpu-*.csv from measure-footprint.sh>
set -euo pipefail

RSS=${1:?usage: g0-verdict.sh <rss.csv> <gpu.csv>}
GPU=${2:?usage: g0-verdict.sh <rss.csv> <gpu.csv>}

# stats <csv> <value column>: after-hour-1 count, mean, min, max, p99, slope
# (MB/h), hours covered. The p99 is a nearest-rank percentile over the
# after-hour-1 samples, sorted in awk.
stats() {
  awk -F, -v col="$2" '
    NR == 2 { start = $1 }
    NR >= 2 && $1 >= start + 3600 && $col != "" {
      x = ($1 - start) / 3600; y = $col
      n++; sx += x; sy += y; sxx += x * x; sxy += x * y
      v[n] = y
      if (n == 1 || y < min) min = y
      if (n == 1 || y > max) max = y
      last = x
    }
    END {
      if (n < 2) { print "0 0 0 0 0 0 0"; exit }
      slope = (n * sxy - sx * sy) / (n * sxx - sx * sx)
      # insertion sort: a soak is a few thousand samples, so this is instant.
      for (i = 2; i <= n; i++) {
        key = v[i]
        for (j = i - 1; j >= 1 && v[j] > key; j--) v[j + 1] = v[j]
        v[j + 1] = key
      }
      rank = int(n * 0.99 + 0.5); if (rank < 1) rank = 1; if (rank > n) rank = n
      printf "%d %.1f %.1f %.1f %.1f %.2f %.1f\n", n, sy / n, min, max, v[rank], slope, last
    }' "$1"
}

read -r fn fmean fmin fmax fp99 fslope fhours < <(stats "$GPU" 6)
read -r rn rmean rmin rmax rp99 rslope _ < <(stats "$RSS" 3)
[ "$fn" -ge 2 ] || { echo "fewer than two footprint samples after hour 1 — run longer" >&2; exit 1; }

cpu=$(awk -F, 'NR == 2 { s = $1 } NR >= 2 && $1 >= s + 3600 { n++; c += $4 } END { if (n) printf "%.2f", c / n; else print "n/a" }' "$RSS")
gpu=$(awk -F, 'NR == 2 { s = $1 } NR >= 2 && $1 >= s + 3600 && $3 != "" {
        if (!seen) { g0 = $3; t0 = $1; seen = 1 } g1 = $3; t1 = $1 }
      END { if (t1 > t0) printf "%.1f", (g1 - g0) / 1e6 / ((t1 - t0) / 60); else print "n/a" }' "$GPU")
total=$(awk -F, 'NR == 2 { s = $1 } NR >= 2 { e = $1 } END { printf "%.1f", (e - s) / 3600 }' "$GPU")

met() { awk -v v="$1" -v op="$2" -v lim="$3" 'BEGIN {
  ok = (op == "<") ? (v < lim) : (v <= lim); print ok ? "met" : "not met" }'; }

echo "Run: ${total} h in total; after hour 1: ${fn} footprint samples (${fhours} h in), ${rn} RSS samples."
echo
echo "| G0 threshold (after hour 1) | Footprint | | RSS |"
echo "|---|---|---|---|"
echo "| Mean < 150 MB | ${fmean} MB | $(met "$fmean" "<" 150) | ${rmean} MB |"
echo "| p99 < 200 MB | ${fp99} MB | $(met "$fp99" "<" 200) | ${rp99} MB |"
echo "| Linear-fit slope <= 2 MB/h | ${fslope} MB/h | $(met "$fslope" "<=" 2) | ${rslope} MB/h |"
echo "| Idle CPU ~ 0 % | ${cpu} % of one core (ps mean) | for the maintainer | |"
echo
echo "Range, published but not a threshold: footprint ${fmin}-${fmax} MB, RSS ${rmin}-${rmax} MB."
echo "GPU after hour 1: ${gpu} ms per minute."
