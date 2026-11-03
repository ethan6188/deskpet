//! Contact sheets: one PNG showing a pet from several angles, through every
//! clip, and with its procedural motion, so an agent (or a person) can judge
//! it at a glance. This is the "look" step of draft, render, look, refine.

use std::path::Path;
use std::time::Duration;

use ab_glyph::{Font, FontVec, PxScale, ScaleFont};
use glam::{Quat, Vec2};

use crate::behavior;
use crate::model::Model;
use crate::pet::{Pet, Style};
use crate::pose::{self, Pose};
use crate::renderer::{FrameInput, Renderer};
use crate::scene;
use crate::splat;

/// Backdrop behind the pet: mid gray, so both dark ink and white fur read.
const BACKDROP: [u8; 3] = [58, 60, 70];
const LABEL_BAND: u32 = 30;
const GAP: u32 = 6;

pub struct SheetOptions {
    /// Style names to render; empty means every style the pet has.
    pub styles: Vec<String>,
    /// Clip names to render; empty means every clip.
    pub clips: Vec<String>,
    /// Frames per clip row and angles in the turntable row.
    pub columns: usize,
    /// Cell height in pixels. Use the window height times 2 to judge Retina detail.
    pub cell_height: u32,
}

/// A rendered cell before it is placed on the sheet.
struct Cell {
    label: String,
    rgba: Vec<u8>,
}

pub struct Report {
    pub width: u32,
    pub height: u32,
    pub cells: usize,
    pub median_frame: Duration,
}

/// Renders the contact sheet to `out`.
pub fn render(pet: &Pet, model: &Model, opts: &SheetOptions, out: &Path) -> Result<Report, String> {
    let style_names: Vec<String> = if opts.styles.is_empty() {
        pet.manifest.styles.keys().cloned().collect()
    } else {
        opts.styles.clone()
    };
    let clips: Vec<usize> = if opts.clips.is_empty() {
        (0..model.clips.len()).collect()
    } else {
        opts.clips
            .iter()
            .map(|n| {
                model
                    .clip(n)
                    .ok_or_else(|| format!("no clip \"{n}\" in the model"))
            })
            .collect::<Result<_, _>>()?
    };
    let facing = Quat::from_rotation_y(pet.manifest.facing.to_radians());
    let height = scene::model_height(model);
    let cols = opts.columns.max(1);
    let idle = pet
        .manifest
        .animations
        .idle
        .as_deref()
        .and_then(|n| model.clip(n));
    let mut rows: Vec<(String, Vec<Cell>)> = Vec::new();
    let mut frame_times = Vec::new();
    // Every row shares one cell width, the widest any style needs; each
    // style's camera widens its view to match, so nothing is stretched.
    let mut styles = Vec::new();
    for name in &style_names {
        let style: Style = pet.style(Some(name))?;
        let splats = splat::for_style(&pet.dir, model, &style)?;
        let camera = scene::framing(model, &style, facing, scene::splat_reach(&splats));
        styles.push((name, style, splats, camera));
    }
    let aspect = styles.iter().map(|s| s.3.aspect).fold(0.2, f32::max);
    let cell_w = ((opts.cell_height as f32 * aspect).round() as u32).max(16);

    for (name, style, splats, mut camera) in styles {
        camera.aspect = aspect;
        let mut renderer = pollster::block_on(Renderer::offscreen(
            cell_w,
            opts.cell_height,
            model,
            style.clone(),
            &splats,
            camera,
            height,
        ))?;
        let mut draw = |pose: &Pose, yaw: f32, time: f32, gaze: Vec2| -> Result<Vec<u8>, String> {
            let joints = pose::joint_matrices(model, pose);
            let input = FrameInput {
                model: scene::placement(yaw, facing, 0.0),
                joints: &joints,
                morphs: &pose.morphs,
                time,
                gaze,
            };
            let (rgba, t) = renderer.capture(&input)?;
            frame_times.push(t);
            Ok(rgba)
        };
        let rest_or_idle = || {
            let mut p = Pose::rest(model);
            if let Some(c) = idle {
                pose::sample(model, &model.clips[c], 0.0, &mut p);
            }
            p
        };

        // Turntable: the idle pose all the way round, starting from the facing view.
        let mut cells = Vec::new();
        for k in 0..cols {
            let yaw = std::f32::consts::TAU * k as f32 / cols as f32;
            cells.push(Cell {
                label: format!("{:.0}°", yaw.to_degrees()),
                rgba: draw(&rest_or_idle(), yaw, 0.0, Vec2::ZERO)?,
            });
        }
        rows.push((format!("{name}: turntable"), cells));

        for &c in &clips {
            let clip = &model.clips[c];
            let mut cells = Vec::new();
            for k in 0..cols {
                // Spread frames over the clip, including the last, to show the loop point.
                let t = clip.duration * k as f32 / (cols - 1).max(1) as f32;
                let mut p = Pose::rest(model);
                pose::sample(model, clip, t, &mut p);
                cells.push(Cell {
                    label: format!("{t:.2}s"),
                    rgba: draw(&p, 0.0, t, Vec2::ZERO)?,
                });
            }
            rows.push((
                format!("{name}: {} ({:.2}s)", clip.name, clip.duration),
                cells,
            ));
        }

        // Procedural layer: where the head looks, and the blink at its most closed.
        let rig = &pet.manifest.rig;
        let mut cells = Vec::new();
        if let Some(head) = rig.head.as_deref().and_then(|n| model.node(n)) {
            let limit = rig.look_limit.to_radians();
            for (label, gaze) in [
                ("look left", Vec2::new(-1.0, 0.0)),
                ("look right", Vec2::new(1.0, 0.0)),
                ("look up", Vec2::new(0.0, 1.0)),
                ("look down", Vec2::new(0.0, -1.0)),
            ] {
                let mut p = rest_or_idle();
                behavior::look(model, &mut p, head, limit, gaze, facing);
                cells.push(Cell {
                    label: label.into(),
                    rgba: draw(&p, 0.0, 0.0, gaze)?,
                });
            }
        }
        let blink = rig
            .blink
            .as_deref()
            .and_then(|n| model.morph(n))
            .or_else(|| model.morph(crate::eyes::BLINK_MORPH));
        if let Some(m) = blink {
            let mut p = rest_or_idle();
            p.morphs[m] = behavior::blink_weight(0.064).unwrap_or(1.0);
            cells.push(Cell {
                label: "blink".into(),
                rgba: draw(&p, 0.0, 0.0, Vec2::ZERO)?,
            });
        }
        if !cells.is_empty() {
            rows.push((format!("{name}: rig"), cells));
        }
    }

    let width = GAP + cols as u32 * (cell_w + GAP);
    let row_h = LABEL_BAND + opts.cell_height + LABEL_BAND / 2 + GAP;
    let height_px = GAP + rows.len() as u32 * row_h;
    let mut sheet = Canvas::new(width, height_px, [30, 31, 36]);
    let font = load_font();
    for (r, (title, cells)) in rows.iter().enumerate() {
        let y0 = GAP + r as u32 * row_h;
        sheet.text(font.as_ref(), GAP + 2, y0 + 4, title, 18.0, [235, 235, 240]);
        for (c, cell) in cells.iter().enumerate() {
            let x0 = GAP + c as u32 * (cell_w + GAP);
            sheet.cell(x0, y0 + LABEL_BAND, cell_w, opts.cell_height, &cell.rgba);
            sheet.text(
                font.as_ref(),
                x0 + 4,
                y0 + LABEL_BAND + opts.cell_height + 2,
                &cell.label,
                13.0,
                [170, 172, 182],
            );
        }
    }
    image::save_buffer(out, &sheet.px, width, height_px, image::ColorType::Rgba8)
        .map_err(|e| format!("{}: {e}", out.display()))?;

    frame_times.sort();
    Ok(Report {
        width,
        height: height_px,
        cells: rows.iter().map(|r| r.1.len()).sum(),
        median_frame: frame_times
            .get(frame_times.len() / 2)
            .copied()
            .unwrap_or_default(),
    })
}

/// A system font for labels. Labels are a nicety: without a font, no text.
fn load_font() -> Option<FontVec> {
    [
        "/System/Library/Fonts/SFNSMono.ttf",
        "/System/Library/Fonts/Menlo.ttc",
    ]
    .iter()
    .find_map(|p| FontVec::try_from_vec(std::fs::read(p).ok()?).ok())
}

struct Canvas {
    w: u32,
    h: u32,
    px: Vec<u8>,
}

impl Canvas {
    fn new(w: u32, h: u32, bg: [u8; 3]) -> Self {
        let px = (0..w * h)
            .flat_map(|_| [bg[0], bg[1], bg[2], 255])
            .collect();
        Self { w, h, px }
    }

    /// Places a premultiplied RGBA cell over the backdrop.
    fn cell(&mut self, x0: u32, y0: u32, w: u32, h: u32, rgba: &[u8]) {
        for y in 0..h {
            for x in 0..w {
                let s = ((y * w + x) * 4) as usize;
                let d = (((y0 + y) * self.w + x0 + x) * 4) as usize;
                let a = rgba[s + 3] as u32;
                for c in 0..3 {
                    let over = rgba[s + c] as u32 + BACKDROP[c] as u32 * (255 - a) / 255;
                    self.px[d + c] = over.min(255) as u8;
                }
            }
        }
    }

    fn text(&mut self, font: Option<&FontVec>, x: u32, y: u32, s: &str, size: f32, color: [u8; 3]) {
        let Some(font) = font else { return };
        let scaled = font.as_scaled(PxScale::from(size));
        let mut pen = x as f32;
        let baseline = y as f32 + scaled.ascent();
        for ch in s.chars() {
            let id = scaled.glyph_id(ch);
            let glyph = id.with_scale_and_position(size, ab_glyph::point(pen, baseline));
            pen += scaled.h_advance(id);
            let Some(outline) = font.outline_glyph(glyph) else {
                continue;
            };
            let b = outline.px_bounds();
            outline.draw(|gx, gy, cov| {
                let (px, py) = (b.min.x as i64 + gx as i64, b.min.y as i64 + gy as i64);
                if px < 0 || py < 0 || px >= self.w as i64 || py >= self.h as i64 {
                    return;
                }
                let d = ((py as u32 * self.w + px as u32) * 4) as usize;
                for (dst, src) in self.px[d..d + 3].iter_mut().zip(color) {
                    let old = *dst as f32;
                    *dst = (old + (src as f32 - old) * cov.min(1.0)) as u8;
                }
            });
        }
    }
}
