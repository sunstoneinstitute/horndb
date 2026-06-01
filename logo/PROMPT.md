# Image-generation prompts — HornDB metal horns

Prompts for generating renditions of the HornDB mark (the "sign of the horns" /
rock-on hand) with text-to-image models. The canonical mark lives in this
directory as flat SVG; these prompts are for exploring alternative renderings
(stylised, 3D, emblem). See `README.md` for the brand colours.

Brand colours to reference in words (models are unreliable with raw hex):

- Rust-oxide burnt orange — `#C0501C`
- Near-black charcoal — `#14141A`

## Primary — flat vector logo (matches the canonical mark)

> Minimalist flat vector logo of a hand throwing the "sign of the horns" /
> rock-on gesture — a closed fist with the index finger and pinky finger raised,
> middle and ring fingers folded down, thumb tucked across the front. Bold,
> geometric, even-thickness fingers with softly rounded tips. Solid single-colour
> fill in rust-oxide burnt orange (#C0501C). Clean negative space between
> fingers. Iconic, simple, instantly readable at small sizes, app-icon style.
> Centered, flat 2D, no gradient, no shading, no outline, plain white background.
> Logo design, corporate identity, crisp edges.

Negative prompt:

> realistic skin, photorealism, 3D, drop shadow, gradient, texture, fingernails,
> wrinkles, multiple hands, extra fingers, text, letters, watermark, busy
> background, glow, bevel

## Variant A — cold chrome / hardware ("wicked fast")

> 3D render of a chrome metal hand throwing the horns (index and pinky raised,
> middle and ring folded, thumb across), polished reflective stainless steel with
> sharp specular highlights, sitting on a dark charcoal background (#14141A),
> studio lighting, product-shot, octane render, machined-metal logo emblem,
> centered.

## Variant B — heavy-metal emblem

> Heavy-metal band emblem: a fist throwing the sign of the horns, rust-orange and
> weathered iron, surrounded by a circular badge, distressed grunge texture,
> embossed metal, aggressive but clean, sticker / patch design, dark background.

## Usage notes

- Reinforce hex codes with words ("rust-oxide / burnt-orange",
  "near-black charcoal") — models do not parse `#RRGGBB` reliably.
- Hands are a common failure mode (extra or fused fingers). Generate a batch
  (8+) and select; the negative prompt reduces but does not eliminate this.
- For a usable logo, request a plain solid background and centered composition,
  then knock out the background and vector-trace the result.
- Aspect ratio: square (1:1) for the icon; 3:1 or 16:9 for a horizontal lockup.

### Model-specific phrasing

- **Midjourney:** append flags, e.g. `--ar 1:1 --style raw --no text, extra fingers`.
  Midjourney ignores a separate negative-prompt field; use `--no`.
- **DALL·E 3 / GPT-image:** drop the negative-prompt block and instead state
  exclusions inline ("…flat 2D with no gradient or shadow, on a plain white
  background"). These models follow natural-language instructions well.
- **SDXL / local:** use the negative prompt as-is; consider token weighting such
  as `(sign of the horns:1.3)` and `(flat vector logo:1.2)`.
