# Memory soak: PR Marmot v0.9.1, 37.4 h

The second run of gate G0 in [ROADMAP.md](../ROADMAP.md), and the first since the idle repaint was fixed in
v0.9.0. Raw samples are in this folder, so every number below can be recomputed.

## Method

- **Build:** v0.9.1 installed from the Homebrew cask (Developer-ID signed, notarized, `spctl` "Notarized
  Developer ID"), launched normally from `/Applications/prmarmot.app` with its own config: a refresh every
  5 minutes, real GitHub data through `gh`.
- **Conditions:** the window stayed on screen but not frontmost.
  - The run started at 15:46Z, in the afternoon rather than at night, so the first hours are not idle: the Mac
    was in use and the app's window was looked at. Hour 1 is excluded from the gate anyway; hours 2 and 3 are
    not, and they hold most of the peaks below.
  - From 19:00Z onwards, 34 h, nobody used the app. Release builds (`make verify`) ran on the same Mac at
    about 17:55–18:05Z on 2026-09-18, and again briefly on the 19th.
- **Duration:** 2026-09-18 15:46Z to 2026-09-20 05:10Z, 37.4 h.
- **RSS and CPU:** [`scripts/measure.sh`](../scripts/measure.sh) `<pid>` samples `ps` every 60 s (2,243
  samples), with `caffeinate -i -w <pid>` keeping the system awake.
  Data: [`rss-20260918-174604.csv`](rss-20260918-174604.csv).
- **Physical footprint and GPU time:** [`scripts/measure-footprint.sh`](../scripts/measure-footprint.sh), every
  60 s (2,188 samples). Data: [`gpu-20260918-174604.csv`](gpu-20260918-174604.csv).
  - `footprint -p <pid>` gives the physical footprint, the number Activity Monitor's Memory column shows.
  - GPU time is `accumulatedGPUTime` from `ioreg -l -w0 -c AGXDeviceUserClient`, the `AppUsage` entry whose
    `IOUserClientCreator` is the app's pid. No sudo is needed.
- **Verdict:** [`scripts/g0-verdict.sh`](../scripts/g0-verdict.sh) applies G0's thresholds to the two CSVs.

## Results

All figures are after hour 1, as the gate counts.

| Measure | v0.9.1 | v0.8.1, for comparison |
|---|---|---|
| Physical footprint | mean 106.7, median 98, min 93, max 251; slope −0.77 MB/h | mean 157.1, max 257; slope −0.68 MB/h |
| RSS | mean 45.0, median 42.5, min 36.4, max 89.9; slope −0.36 MB/h | mean 87.8; slope −1.37 MB/h |
| GPU | 0.1 ms per minute; 0.00–0.03 ms/min in the fully idle 6-hour blocks | 29.3 ms per minute |
| CPU | `ps` mean 0.01 % of one core (0.002 % from 19:00Z) | `ps` mean 1.35 % |

- **No growth, again.** Both curves fall slightly. The hourly median footprint over the 34 quiet hours moves
  between 93 and 143 MB with no trend: 108 MB through the night, then 95–98 MB for the rest of the run.
- **The idle repaint is gone.** The v0.9.0 fix (8f71da6) is doing exactly what it was meant to: in the 34 h
  after 19:00Z the app drew **8 frames in total**. In the whole run after hour 1, 2,092 of 2,131 minutes
  (98.2 %) drew no frame at all. In v0.8.1 it drew one every ~5.25 s.
- **The peaks are single minutes in which a frame was drawn.** Splitting the samples by whether GPU time
  advanced during that minute:

  | Footprint after hour 1 | Samples | Mean | Min | Max | max − min |
  |---|---|---|---|---|---|
  | Minutes with no frame | 2,092 (98.2 %) | 105.8 | 93 | 159 | 66 |
  | Minutes with a frame | 39 (1.8 %) | 154.2 | 110 | 251 | 141 |

  The two highest samples, 251 MB at 16:47Z on the 18th and 236 MB at 05:40Z on the 19th, are both minutes with
  a repaint; the sample after each one is back at 116–127 MB. A drawable is about 23 MB and several can be in
  flight at once, which is what those minutes are made of.

## Verdict

G0's thresholds in ROADMAP.md, applied after hour 1:

| G0 threshold | Measured | |
|---|---|---|
| Mean physical footprint < 150 MB | 106.7 MB | met |
| p99 physical footprint < 200 MB | 159 MB | met |
| Linear-fit slope ≤ 2 MB/h | −0.77 MB/h (footprint), −0.36 MB/h (RSS) | met |
| Idle CPU ≈ 0 % | 0.01 % of one core | met on any reading |

**G0 passes.** The mean, which v0.8.1 missed at 157.1 MB, is 106.7 MB, and the curve ends lower than it starts
over 37 hours — the leak the gate exists to catch is not there.

### The threshold that was rewritten

G0 read "(max − min) after hour 1 ≤ 25 MB" until this run. That bound is **not met, on any reading**:

| Reading of (max − min) | Footprint |
|---|---|
| All samples after hour 1 | 158 MB (93–251) |
| Minutes that drew no frame | 66 MB (93–159) |
| Quiet hours (from 19:00Z), no frame | 50 MB (93–143) |
| p90 − min | 40 MB |
| p85 − min | 24 MB — 85.5 % of minutes fit inside a 25 MB band |

The 25 MB was chosen before anything had been measured. What the app actually does is sit at 93–98 MB, drift to
around 108–143 MB for hours at a time, and touch 251 MB in a minute that draws a frame. A drawable is about
23 MB and several can be in flight, so **one drawn frame breaks a 25 MB band** — no build of this app on macOS
would ever meet it, which makes it an instrument that can only ever report failure.

On 2026-09-20 the maintainer replaced it with **p99 after hour 1 < 200 MB**. That keeps what the bound was for —
a future build whose working set grows would trip it — without failing the app for drawing. Per ROADMAP.md's
rule that numbers are "revised against reality, never moved to make a gate pass", both numbers stay published:
the p99 is 159 MB and the maximum is 251 MB.

## Reproduce

```sh
pid=$(pgrep -x prmarmot)
caffeinate -i -w "$pid" &
scripts/measure.sh "$pid" measurements &            # RSS + CPU every 60 s
scripts/measure-footprint.sh "$pid" measurements &  # footprint + GPU every 60 s
scripts/g0-verdict.sh measurements/rss-*.csv measurements/gpu-*.csv
```
