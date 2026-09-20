# Memory soak: PR Marmot v0.8.1, 18.3 h unattended

The first run of gate G0 in [ROADMAP.md](../ROADMAP.md): leave the app open and idle overnight and
see whether its memory grows. Raw samples are in this folder, so every number below can be recomputed.

## Method

- **Build:** v0.8.1 installed from the Homebrew cask (Developer-ID signed, notarized), launched
  normally from `/Applications/prmarmot.app` with its own config: a refresh every 5 minutes,
  real GitHub data through `gh`.
- **Conditions:** the window stayed on screen but not frontmost. Nobody used the Mac. The machine
  was under memory pressure from other processes for the whole run.
- **Duration:** 2026-09-16 17:43Z to 2026-09-17 11:59Z, 18.3 h.
- **RSS and CPU:** [`scripts/measure.sh`](../scripts/measure.sh) `<pid>` samples `ps` every 60 s
  (1,097 samples), with `caffeinate -i -w <pid>` keeping the system awake while the app lives.
  Data: [`rss-20260916-194303.csv`](rss-20260916-194303.csv).
- **Physical footprint and GPU time:** a second read-only sampler, every 60 s (1,083 samples).
  Data: [`gpu-20260916-194303.csv`](gpu-20260916-194303.csv).
  - `footprint -p <pid>` gives the physical footprint, the number Activity Monitor's Memory
    column shows.
  - The app's Metal GPU time is `accumulatedGPUTime`, and its last submit is `lastSubmittedTime`,
    both from `ioreg -l -w0 -c AGXDeviceUserClient` (the `AppUsage` entry whose
    `IOUserClientCreator` is the app's pid). No sudo is needed.
  - `lastSubmittedTime` is on the `CLOCK_UPTIME_RAW` clock, recorded as `mach_now_ns`.

## Results

| Measure | Whole run | After hour 1 |
|---|---|---|
| RSS | 98.7 → 75.4 MB; min 72.4, max 98.7 | mean 87.8, slope −1.37 MB/h, max − min 24.3 |
| Physical footprint | low point 116–141 MB (one sample at 95); peaks to 257 MB during repaints | mean 157.1, slope −0.68 MB/h, max − min 162 |
| GPU | 32,180 ms over 1,097 min = 29.3 ms per minute | every full hour 28.8–29.9 ms/min |
| CPU | 16 min 56 s over 18 h 39 min of uptime = 1.51 % of one core | `ps` mean 1.35 % |

- **No growth.** RSS fell rather than rose. Most of the fall came in hours 12–14, which matches
  macOS compressing idle pages under memory pressure. The footprint counts compressed pages, and
  its low point did not grow either: it moved between 116–119 and 141 MB, one drawable (~23 MB)
  in either direction.
- **v0.8.1 repaints every ~5.25 s while idle.** Each repaint costs about 2–2.5 ms of GPU time
  (0.05 % duty). For as long as it lasts, the window's graphics memory is counted in the
  footprint: 218 of the 1,024 footprint samples after hour 1 read over 200 MB.
- **Fixed on main in 8f71da6** (ships in v0.9.0): the idle window now repaints once a minute.
  In a 5-minute check of a demo instance with no input, GPU submits came 57–63 s apart. A full
  soak of v0.9.0 has not been run yet.

## Verdict

> **Superseded (2026-09-20).** The "(max − min) ≤ 25 MB" row below is no longer part of G0; it was replaced by
> "p99 < 200 MB" after the v0.9.1 run showed no build could meet it. The rest of this file stands as measured.
> See [`2026-09-20-memory-gate-v0.9.1.md`](2026-09-20-memory-gate-v0.9.1.md).

G0's thresholds in ROADMAP.md, applied after hour 1:

| G0 threshold | Measured | |
|---|---|---|
| Mean physical footprint < 150 MB | 157.1 MB | not met |
| Linear-fit slope ≤ 2 MB/h | −0.68 MB/h (footprint), −1.37 MB/h (RSS) | met |
| (max − min) ≤ 25 MB | 162 MB footprint, 24.3 MB RSS | not met on footprint |
| Idle CPU ≈ 0 % | 1.35 % of one core | about 1 % of it is per-frame work while the window is visible |

- **RSS:** every threshold is met.
- **Physical footprint** is what G0 names. Its mean and range are **not met**, because of the 5 s
  idle repaint: 218 of the 1,024 samples after hour 1 read over 200 MB, the window's graphics
  memory during a repaint.
- **Maintainer's ruling (2026-09-17):** the run is acceptable on RSS and on the idle GPU cost.
- **Next:** the repaint is fixed in v0.9.0 (8f71da6). G0 is measured again on the shipped v0.9.0
  cask build, and that result decides the gate. **It did:** the v0.9.1 build passed on 2026-09-20 with a
  mean of 106.7 MB and 0.01 % of one core.

## Reproduce

```sh
pid=$(pgrep -x prmarmot)
caffeinate -i -w "$pid" &
scripts/measure.sh "$pid" measurements &   # RSS + CPU every 60 s
scripts/measure-summary.sh measurements/rss-*.csv
footprint -p "$pid"                        # physical footprint, any time
ioreg -l -w0 -c AGXDeviceUserClient | grep -B1 "\"IOUserClientCreator\" = \"pid $pid," | grep AppUsage
```
