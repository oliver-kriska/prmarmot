# PR Marmot branding

The production identity is the upright Tatra sentinel: amber marmot, granite
rock, slate-blue tile. Display name: **PR Marmot**. Machine slug: `prmarmot`.
These SVGs are original vector artwork based on the selected Alpine sentinel
direction, not embedded or traced Painter bitmaps.

## Sources and exports

- `icon.svg`: app tile, source for `icon-{64,128,256,512,1024}.png`.
- `icon-small.svg`: optically enlarged and simplified app tile, source for
  `icon-{16,32}.png`. **Use this source at physical export sizes ≤32px.**
- `mark.svg`: single-color untiled logo mark, for display at 48px or larger.
- `mark-small.svg`: single-color untiled optical mark for 16–32px. Both mark
  SVGs use `currentColor` (black by default); set the SVG color when embedding,
  or use the explicit light/dark PNG exports in native image contexts.
- `mark-light-{16,32}.png`: dark ink for light surfaces.
- `mark-dark-{16,32}.png`: light ink for dark surfaces.
- `logo-light.svg` / `logo-dark.svg`: transparent horizontal **PR Marmot**
  lockups, dark ink for light backgrounds and light ink for dark backgrounds.
  Matching PNGs are 640×200. Lettering is outlined DejaVu Sans; no installed
  font or network resource is needed. Keep `FONT-LICENSE.txt` with the assets.

The small tile intentionally drops the eye, paw line, gradients, and rock
facets. Do not derive monochrome icons by thresholding the shaded app tile.
Use the monochrome sources, not a dark-mode recoloring of the full tile.
The same slate app tile works on both light and dark desktop backgrounds.

## Integration and validation

Run `scripts/generate-icons.sh` on macOS to regenerate the seven app PNGs and
`prmarmot.icns` with librsvg and Apple's `iconutil`. The script selects
`icon-small.svg` for 16 and 32 physical pixels and `icon.svg` above 32.
Normal builds consume the generated files without requiring these tools.

Logo and monochrome PNG exports were rasterized with CairoSVG 2.9.1; app PNGs
are regenerated with librsvg. All seven app exports have RGBA transparency
and the requested dimensions. The SVGs contain no
text elements, embedded images, or external assets. Actual 16/32/64/128px
exports and enlarged 16px pixels were visually inspected on white and
`#1A1B1E` surfaces, alongside the two outlined lockups. The small silhouette
stays connected; species detail is intentionally carried by the larger mark
and the name rather than a subpixel eye or claw.

The generated ICNS was decoded and rendered by macOS AppKit from a staged
app bundle on light and dark backgrounds. Installed Dock/Finder cache refresh
is a separate installation check. Stage verification bundles; do not overwrite
a running app.
