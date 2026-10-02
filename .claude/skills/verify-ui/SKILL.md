---
name: verify-ui
description: Screenshot-verify what the running prboard GPUI window actually renders. Use this EVERY time visual confirmation is needed — after any UI/theme/layout change, before claiming a visual fix works, when checking light vs dark mode, or when a screenshot looks stale or the app looks "hung". Never verify prboard visuals with a bare screencapture or a full-screen shot; this skill exists because the naive approaches silently capture wrong or stale pixels.
---

# Verify prboard UI by screenshot

## Why the naive approach fails (all three learned the hard way)

1. **GPUI does not repaint occluded windows.** A window behind another window
   (or on another Space) keeps its last frame — often the *first* frame, which
   shows "Loading board…" and looks exactly like a hung fetch. The app is fine;
   the pixels are stale. The window must be frontmost before capture.
2. **Multiple prboard instances share one app name.** A dev binary, the
   installed .app, and a measurement instance can all run at once; capturing
   "the prboard window" by name grabs an arbitrary one. Always select the
   window by **PID**.
3. **Keystroke automation is dangerous.** Sent keys land in whichever window
   has focus at that instant; a focus race once delivered `q` to the
   memory-gate instance and killed an overnight measurement. **While a gate
   run is live (check for a `measure.sh` process), send no keystrokes at
   all.** Outside gate runs, re-activate the target window immediately before
   *each* keystroke, never once for a batch.

## Procedure

1. Find the PID of the instance you mean:
   ```sh
   pgrep -lf prboard    # distinguish target/debug, target/release, and prboard.app paths
   ```
2. Capture (activates the window by PID, resolves its CGWindowID, screenshots
   just that window):
   ```sh
   .claude/skills/verify-ui/scripts/capture-window.sh <pid> "$SCRATCHPAD/ui.png"
   ```
3. **Read the PNG** with the Read tool and actually look at it. Check against
   `DESIGN.md` (palette, dots-not-emoji, calm rule) — not just "did it render".
4. For theme comparisons, capture once per theme; for before/after, keep both
   files and name them (`before-*.png` / `after-*.png`).

On demo data, `make capture` (`scripts/capture.sh`) does all of this for the
board, review queue, All open and a Details panel, in both themes. To press keys
in an instance you launched, post them to its PID rather than typing into the
focused window: `swiftc -O scripts/capture/post-event.swift -o "$SCRATCHPAD/post-event"`,
then `"$SCRATCHPAD/post-event" <pid> key:125 key:49` (Down, Space). Posted
clicks do not make GPUI clicks, so a scene behind a button needs a key.

Launching a throwaway instance for verification is fine (`./target/debug/prboard
--repo owner/name`) — quit it with a mouse click on close or `kill <pid>`,
never a synthesized `q`, and leave any measured instance untouched.

Needs macOS Screen Recording permission for the terminal (screencapture
silently produces empty/desktop images without it) and `swift` on PATH
(ships with Xcode).
