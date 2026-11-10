# Regenerate the README hero GIF

`assets/screenshots/hero.gif` is an 11-second, 640×360 loop cut from the
finished 16:9 showcase film. It uses fictional demo data. The original
`assets/screenshots/board.png` stays available as a still.

## Source and cuts

The source is `prmarmot-film-16x9-v4c-4k.mp4` (3840×2160, 60 fps,
49.433 seconds). It is a local production asset, not included in this
repository. Obtain that exact film before regenerating; the square social
cut has different framing and timing.

| Source interval | GIF interval | Content |
| --- | --- | --- |
| 10.200–12.200 s | 0–2 s | Full board, then the zoom into Needs action. The first frame is a finished board, not a transition or title card. |
| 14.400–18.400 s | 2–6 s | Details opens on #1842: approved, but CI failing and two comments unresolved. |
| 20.500–25.500 s | 6–11 s | “Back from a meeting: what moved.” The refresh adds blue markers beside #3304 and #425. |

The source intervals and their playback timing are unchanged from the first
hero GIF. Details is cropped to the pane; the refreshed board is cropped to
the PR column and titles so its blue dots remain visible at phone width.
Both crops retain the original caption/disclosure strip beneath them.
The overview and meeting interstitial stay full frame. The last interval
is split at source 22.000 s only to apply the crop after the interstitial;
no source time is removed or repeated. No intro, end card, audio, or newly
drawn text is included.

Reducing the original to 10 fps alone gave 6.54 MB; 800 px wide gave 4.91 MB.
The 128-color/diff/Bayer-5 palette was already in use. Tighter crops fixed
phone-width readability but still left about 4.9 MB; even 720 px gave
4.42 MB. The first checked candidate under 4 MB used the crops at 640 px.

## Encode

Run from the repository root with FFmpeg (generated with 9.0.2). Set `SOURCE`
to the film's location if it differs from the local production path below.
The command replaces the generated GIF.

```sh
SOURCE=.claude/scratchpad/showcase-v3/prmarmot-film-16x9-v4c-4k.mp4
POST='hqdn3d=1.5:1.5:3:3,setpts=PTS-STARTPTS'
FULL="fps=10,scale=640:360:flags=lanczos,${POST}"
CAPTION='crop=3840:280:0:1880,scale=640:46:flags=lanczos'

ffmpeg -hide_banner -loglevel warning \
  -ss 10.2 -t 2 -i "$SOURCE" \
  -ss 14.4 -t 4 -i "$SOURCE" \
  -ss 20.5 -t 1.5 -i "$SOURCE" \
  -ss 22 -t 3.5 -i "$SOURCE" \
  -filter_complex "\
[0:v]${FULL}[a];\
[1:v]fps=10,split[b0][b1];\
[b0]crop=2600:1274:1240:200,scale=640:314:flags=lanczos[bv];\
[b1]${CAPTION}[bc];\
[bv][bc]vstack,setsar=1,${POST}[b];\
[2:v]${FULL}[c];\
[3:v]fps=10,split[d0][d1];\
[d0]crop=2400:1176:0:400,scale=640:314:flags=lanczos[dv];\
[d1]${CAPTION}[dc];\
[dv][dc]vstack,setsar=1,${POST}[d];\
[a][b][c][d]concat=n=4:v=1:a=0,split[frames][colors];\
[colors]palettegen=max_colors=128:stats_mode=diff[palette];\
[frames][palette]paletteuse=dither=bayer:bayer_scale=5:diff_mode=rectangle[out]" \
  -map '[out]' -an -loop 0 -y assets/screenshots/hero.gif
```

The first palette pass samples the assembled sequence; the second maps its
frames to that shared palette. Ten frames per second, light denoising,
128 palette colors, and ordered dithering keep the film's grain from
inflating the GIF while preserving the text and blue markers. Every frame
lasts 100 ms. The palette has 127 rendered opaque colors plus transparency;
FFmpeg pads its stored color table to 256 entries.

## Check the result

```sh
ffprobe -v error \
  -show_entries stream=width,height,nb_frames:format=duration,size \
  -of json assets/screenshots/hero.gif
```

The checked export is 640×360, 110 frames, 11.00 seconds, and 3,442,059 bytes
(3.44 MB; 3.28 MiB), under the 4,000,000-byte budget. The command above was
rerun and reproduced the GIF byte-for-byte. Recheck when changing FFmpeg
versions or encoding settings.

Watch the loop and inspect its first frame, the Details hold around 4 s,
and the change markers around 9 s, including at **360 px display width**.
The phone-width checks passed for the Note, “alex — approved”, the caption,
and the two blue markers. Check that the first frame is useful when paused,
“alex — approved” and both blockers remain readable together,
the blue dots appear only after the refresh, captions are complete, and
the loop ends on the board rather than an end card. The final hard cut
back to the overview deliberately restarts the fictional demo.
