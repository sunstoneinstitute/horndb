# HornDB logo

The HornDB mark is the **sign of the horns** (🤘) — a nod to the name (*Horn* clauses),
to the engine being written in **Rust** (metal → metal horns), and to the project's
"wicked fast" intent. Rendered in rust-oxide orange, a colour no other RDF/DB engine owns.

## Colours

| Token | Hex | Use |
|---|---|---|
| Rust | `#C0501C` | The mark, and the "DB" in the wordmark |
| Ink | `#1A1A20` | The "Horn" in the wordmark (near-black) |
| Dark BG | `#14141A` | App-icon / dark-surface background |
| Cream | `#F3EFE7` | Optional warm light background |

The mark is single-colour: it recolours cleanly to solid `#1A1A20` (or white) when rust
isn't available. Don't add gradients — the flat version is the canonical one.

## Files

| File | What | Use |
|---|---|---|
| `horndb-mark.svg` | Bare mark, tall (82×120), transparent | Hero / general icon |
| `horndb-mark-square.svg` | Mark centred in a square, transparent | Favicon & web-manifest source |
| `horndb-icon-app.svg` | Square mark on dark `#14141A` | iOS / Android / app touch icon |
| `horndb-horizontal.svg` | Mark + "HornDB" wordmark (text outlined to paths) | Headers, READMEs, docs |
| `favicon.ico` | Multi-resolution 16/32/48 | Site favicon |
| `png/favicon-16/32/48.png` | Transparent square rasters | Favicon fallbacks |
| `png/icon-192.png`, `png/icon-512.png` | Transparent square rasters | PWA / web manifest |
| `png/apple-touch-icon-180.png` | Dark-bg square raster | `<link rel="apple-touch-icon">` |
| `png/horndb-mark-512.png` | Tall transparent raster | High-res mark |
| `png/horndb-horizontal@2x.png` (416×140), `@3x.png` (624×210) | Lockup rasters | Where SVG isn't supported |

The wordmark in `horndb-horizontal.svg` is **outlined to vector paths** (originally Arial
Bold), so it renders identically without any font installed.

## Clear space & minimum size

- **Clear space:** keep padding around the mark equal to the width of one raised finger
  (≈ the gap already built into the square/app SVGs).
- **Minimum size:** the mark stays legible down to 16px; below that, prefer a solid
  single-colour rendering.

## Regenerating the rasters

Requires `rsvg-convert` (librsvg) and `magick` (ImageMagick):

```bash
cd logo
rsvg-convert -w 16  horndb-mark-square.svg -o png/favicon-16.png
rsvg-convert -w 32  horndb-mark-square.svg -o png/favicon-32.png
rsvg-convert -w 48  horndb-mark-square.svg -o png/favicon-48.png
rsvg-convert -w 192 horndb-mark-square.svg -o png/icon-192.png
rsvg-convert -w 512 horndb-mark-square.svg -o png/icon-512.png
rsvg-convert -w 180 horndb-icon-app.svg    -o png/apple-touch-icon-180.png
rsvg-convert -w 512 horndb-mark.svg         -o png/horndb-mark-512.png
rsvg-convert -w 416 horndb-horizontal.svg   -o png/horndb-horizontal@2x.png
rsvg-convert -w 624 horndb-horizontal.svg   -o png/horndb-horizontal@3x.png
magick png/favicon-16.png png/favicon-32.png png/favicon-48.png favicon.ico
```
