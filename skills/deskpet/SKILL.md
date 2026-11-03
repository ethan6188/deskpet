---
name: deskpet
description: Make, tune and check 3D desktop pets for the deskpet engine (Rust, macOS). Turns a glTF model into a pet package, renders contact sheets to look at, checks assets against power and quality budgets, and iterates draft, render, look, refine. Use when someone wants a desktop pet, wants a 3D model to live on their Mac desktop, or wants a deskpet pet to look or move better.
compatibility: macOS with a Metal GPU, Rust stable. Run from a deskpet checkout.
---

# deskpet

A pet is a directory: `pet.toml`, a glTF binary (`.glb`) and `LICENSE.md`. The engine draws it at native
Retina resolution in a transparent always-on-top window, in one of two styles: `real` (soft fur-like
shading plus shell fur), `anime` (cel bands plus ink outline) or `splat` (skinned Gaussian splats).

The method: you can't judge a pet from its numbers. Render it, open the PNG, look, change one thing,
render again. `check` catches what eyes miss; the contact sheet catches what checks miss.

## Before anything

1. Work in a deskpet checkout (it has `Cargo.toml` and `pets/fox`). Build once: `cargo build --release`.
   The binary is `./target/release/deskpet`.
2. Check it works: `./target/release/deskpet render pets/fox` writes `pets/fox/contact.png`. Open it with
   your image-reading tool.

## 1. Get a model

deskpet does not generate models. It needs a rigged, animated `.glb`:

- Ask the user for one, or for where to get one. Record its source and license in `LICENSE.md`; `check`
  warns while it still says TODO. Never ship a model whose license you don't know.
- Good models for a pet: under 60k triangles, one skeleton, an idle loop, a few short extra clips, and
  ideally an eyes-closed morph target for blinking.
- A model with no clips still works: it will only breathe. Say so to the user.

## 2. Make the package

```bash
./target/release/deskpet new pets/<name> --model path/to/model.glb --name "<Name>"
```

This copies the model to `model.glb` and writes `pet.toml` with guesses marked `# GUESS`: which clip is
idle, which are fidgets, which reacts to a click, which node is the head, which morph blinks. Every guess
has to be confirmed on the contact sheet.

## 3. Check, render, look

```bash
./target/release/deskpet check pets/<name>          # add --json to parse it
./target/release/deskpet render pets/<name>         # pets/<name>/contact.png
```

`check` exits 1 on errors. Fix every error first, then read the warnings: each says what will look wrong
and why.

The contact sheet has, per style:

- **turntable**: the idle pose every 60°. The first cell is what the user sees. If the pet shows its back
  or side when it should face the viewer, change `facing` (degrees about Y; 90 and -90 are the usual
  fixes). Three-quarter views (around 30°) usually read better than straight on.
- **one row per clip**: frames spread over the clip, first to last. The first and last cell of a loop
  should match. Look for limbs through the body, feet sliding, clips that leave the frame.
- **rig**: the head looking left, right, up, down, and the blink. If the wrong part turns, `rig.head` is
  the wrong node: pick another node. A name that doesn't exist makes `check` list every
  node, clip or morph the model has. Lower `look_limit` if the
  neck twists unnaturally.

Narrow the sheet while iterating: `--style anime`, `--clip Walk`, `--columns 8`, `--cell 480` (cell height
in pixels; 2x the window height shows true Retina detail).

## 4. Tune the style

In `[styles.<name>]`:

| key | style | what it does | start at |
|---|---|---|---|
| `kind` | both | `real` or `anime` | |
| `tint` | both | multiplies the texture | `[1, 1, 1]` |
| `glow` | both | rim light color | `[1.0, 0.9, 0.8]` |
| `fur` | real | shell fur length, fraction of height; 0 is off | `0.01`–`0.02` |
| `outline` | anime | ink width, fraction of height | `0.004`–`0.008` |
| `outline_color` | anime | ink color | near black, warm |
| `bands` | anime | cel light steps | `2` or `3` |

Fur suits soft, dense models; on low-poly ones it reads as fuzz, so keep it short. Outlines that look
broken at creases mean the model has split normals at hard edges; deskpet already welds them for the
outline, so it's usually the width that's too large.

## 4b. Gaussian splats (photographic look)

A `kind = "splat"` style draws a 3DGS point cloud (binary PLY) bound to the model's skeleton instead of the
mesh. The glTF still provides skeleton, clips and click-through.

- Without a capture, `deskpet bake-splats pets/<name> --count 60000` makes one from the textures and adds
  `[styles.splat]`. It proves the pipeline; it doesn't look better than the mesh.
- With a capture (e.g. trained with Brush): crop the background first, copy the `.ply` into the package,
  set `splats = "file.ply"`, then fit `splat_scale`, `splat_rotate` (degrees X, Y, Z) and `splat_offset`
  until the splat turntable lines up with the `real` turntable; `deskpet align-splats pets/<name> --style <style>`
  fits them automatically and writes them into `pet.toml`. `check` says what share of splats sit far
  from the mesh: above 1% the alignment is wrong or background is left.
- Keep under 300k splats; `check` prints the CPU sort time per frame.

## 4c. Eyes

Eyes carry most of a pet's expression. If the model's eyes are painted flat or missing, add `[eyes]`:
`center` (the +x eye, rest-pose model space; `check` prints the head joint's position as a start),
`forward` (where that eye looks), `radius`, `height` (below 1 for almond), `iris` color, `iris_size`,
`pupil`, `pupil_aspect` (small = slit, for foxes and cats). deskpet mirrors them, binds them to
`rig.head`, follows the cursor with the iris, and blinks them without a morph target. Judge them on a
`--cell 1100` sheet: the iris should fill most of the eye, the pupil should read from across the room.

## 5. Behavior

In `[animations]`: `idle` loops; `fidget` clips play now and then while idle; `react` plays on a click.
Pick short clips for fidgets and reactions (under ~2 s). If `check` warns that a clip starts far from
idle, the 0.35 s crossfade will look like sliding: pick another clip or accept it knowingly.

## 6. Try it live

```bash
./target/release/deskpet --pet pets/<name> --style anime
```

Left-click reacts and drags; right-drag spins; Esc quits. Transparent parts let clicks through. You can't
judge interaction from a screenshot: ask the user to try it, and tell them what to look for.

## Budgets

`check` measures them. Keep them, because the pet runs all day:

- frame under 4 ms at Retina size (error over 12 ms);
- under 60k triangles (error over 250k);
- textures at most 4096 px;
- the pet fills at least 15% of its window (less means a clip wanders and every frame draws wasted pixels).

## Done means

`check` has no errors, every remaining warning is explained to the user, every `# GUESS` was confirmed or
changed on a sheet you looked at, and `LICENSE.md` names the source and license.
