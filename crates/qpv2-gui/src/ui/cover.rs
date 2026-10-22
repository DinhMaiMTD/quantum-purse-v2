//! The cover's mark: the qp∞ logo assembling out of dust, with depth.
//!
//! Grains start scattered through a shallow volume behind the mark and fly to
//! their places on it, in the order the logo would be drawn. Depth comes from
//! three cues, never from rotation: perspective (a far grain sits nearer the
//! centre), focus (a grain off the mark's plane is a large, soft, dim disc
//! that sharpens as it lands), and parallax (near dust drifts faster than
//! far dust). The assembly replays each time the cover appears.

use std::f32::consts::{PI, TAU};

use eframe::egui;

use super::utils::{
    draw_qp_logo, qp_infinity_points, QpMarkLayout, QP_RING_RADIUS, QP_STEM_BOTTOM, QP_STEM_GAP,
};
use crate::types::AppColors;

/// Size of the mark once assembled.
const MARK_SIZE: f32 = 72.0;
/// The area the dust lives in, centred on the mark.
const STAGE: egui::Vec2 = egui::vec2(320.0, 150.0);

/// Grains along the ∞, and down each of the two stems.
const LOOP_GRAINS: usize = 380;
const STEM_GRAINS: usize = 60;
/// Dust drifting behind the mark, and the few motes passing in front of it.
const FAR_DUST: usize = 36;
const NEAR_DUST: usize = 5;

// Timeline, in seconds since the cover appeared.
/// The dust fades in over this long.
const FADE_IN: f32 = 0.4;
/// When the first grain leaves, how long each flies, and how long from the
/// first departure to the last, in drawing order.
const FIRST_DEPARTURE: f32 = 0.3;
const FLIGHT: f32 = 0.8;
const SPREAD: f32 = 1.0;
/// The landed grains hand over to the crisp mark between these times.
const LOCK_START: f32 = 2.2;
const LOCK_END: f32 = 2.6;
/// A run of light along the ∞ as it locks, then a fainter one now and then.
const RIPPLE: f32 = 0.7;
const SHIMMER_FIRST: f32 = 4.5;
const SHIMMER_EVERY: f32 = 5.0;

/// The deepest a grain starts, in depth units. A point at depth z is drawn
/// at 1 / (1 + PERSPECTIVE * z) of its distance from the stage centre.
const DEPTH_MAX: f32 = 5.0;
const PERSPECTIVE: f32 = 0.45;

/// Draws the cover's mark in a `STAGE`-sized area: the assembly while it
/// plays, then the still mark with drifting dust, film grain and a passing
/// shimmer.
pub(crate) fn draw_cover_mark(ui: &mut egui::Ui, colors: &AppColors) {
    let now = ui.input(|i| i.time);
    let start = ui
        .ctx()
        .memory_mut(|m| *m.data.get_temp_mut_or_insert_with(start_id(), || now));
    let t = (now - start) as f32;

    let (stage, _) = ui.allocate_exact_size(STAGE, egui::Sense::hover());
    let painter = ui.painter_at(stage);
    let mark_rect = egui::Rect::from_center_size(stage.center(), egui::vec2(MARK_SIZE, MARK_SIZE));
    let layout = QpMarkLayout::new(mark_rect);
    let fade_in = smoothstep(0.0, FADE_IN, t);
    let lock = smoothstep(LOCK_START, LOCK_END, t);

    draw_dust(&painter, stage, colors, t, fade_in, false);
    if t < LOCK_END {
        draw_grains(&painter, stage, &layout, colors, t, fade_in * (1.0 - lock));
    }
    if lock > 0.0 {
        draw_qp_logo(
            &painter,
            mark_rect,
            colors.accent.gamma_multiply(lock),
            colors.bg.gamma_multiply(lock),
        );
    }
    if (LOCK_START..LOCK_START + RIPPLE).contains(&t) {
        draw_light_run(&painter, &layout, colors, (t - LOCK_START) / RIPPLE, 1.0);
    } else if t >= SHIMMER_FIRST {
        let since = (t - SHIMMER_FIRST) % SHIMMER_EVERY;
        let length = RIPPLE * 1.6;
        if since < length {
            draw_light_run(&painter, &layout, colors, since / length, 0.4);
        }
    }
    draw_dust(&painter, stage, colors, t, fade_in, true);
    draw_film_grain(&painter, stage, t);

    // Full frame rate while the mark assembles. After that the motion is
    // slow drift and 24 fps grain, so 30 fps saves power on an idle cover.
    if t < LOCK_START + RIPPLE {
        ui.ctx().request_repaint();
    } else {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(33));
    }
}

/// Replays the assembly the next time the cover is drawn.
pub(crate) fn restart_cover(ctx: &egui::Context) {
    ctx.memory_mut(|m| m.data.remove::<f64>(start_id()));
}

fn start_id() -> egui::Id {
    egui::Id::new("cover-assembly-start")
}

/// A grain's landing spot, in stroke widths from the ∞'s crossing, and when
/// in the drawing order it lands: 0 first, 1 last.
struct Target {
    x: f32,
    y: f32,
    order: f32,
}

/// Landing spots spread evenly along the ∞ (the first three quarters of the
/// order), then down both stems at once, top to bottom (the last quarter).
fn targets() -> Vec<Target> {
    let path = qp_infinity_points();
    let (cum, total) = arc_lengths(&path);
    let n = path.len();
    let mut out = Vec::with_capacity(LOOP_GRAINS + 2 * STEM_GRAINS);

    for g in 0..LOOP_GRAINS {
        let s = (g as f32 + noise(g, 1)) / LOOP_GRAINS as f32 * total;
        let j = cum
            .partition_point(|&c| c <= s)
            .saturating_sub(1)
            .min(n - 1);
        let (a, b) = (path[j], path[(j + 1) % n]);
        let len = (cum[j + 1] - cum[j]).max(1e-6);
        let f = (s - cum[j]) / len;
        let (dx, dy) = ((b.0 - a.0) / len, (b.1 - a.1) / len);
        // Anywhere across the stroke's width, not just its centre line.
        let across = (noise(g, 2) - 0.5) * 0.9;
        out.push(Target {
            x: a.0 + (b.0 - a.0) * f - dy * across,
            y: a.1 + (b.1 - a.1) * f + dx * across,
            order: 0.75 * s / total,
        });
    }

    let top = -(QP_RING_RADIUS + 0.5);
    let stem_x = QP_STEM_GAP / 2.0 + 0.5;
    for (stem, x) in [-stem_x, stem_x].into_iter().enumerate() {
        for m in 0..STEM_GRAINS {
            let i = LOOP_GRAINS + stem * STEM_GRAINS + m;
            let f = (m as f32 + noise(i, 1)) / STEM_GRAINS as f32;
            out.push(Target {
                x: x + (noise(i, 2) - 0.5) * 0.9,
                y: top + (QP_STEM_BOTTOM - top) * f,
                order: 0.75 + 0.25 * f,
            });
        }
    }
    out
}

/// Each grain flies from a random point in depth to its landing spot along
/// a slight arc, coming into focus and flashing white as it lands.
fn draw_grains(
    painter: &egui::Painter,
    stage: egui::Rect,
    layout: &QpMarkLayout,
    colors: &AppColors,
    t: f32,
    alpha: f32,
) {
    let centre = stage.center();
    for (i, target) in targets().iter().enumerate() {
        let landing = layout.point(target.x, target.y) - centre;

        // Start: a random spot that projects inside the stage, at a random
        // depth, so the grains fill the frame before they gather.
        let z0 = 1.5 + noise(i, 3) * (DEPTH_MAX - 1.5);
        let k0 = 1.0 / (1.0 + PERSPECTIVE * z0);
        let angle = noise(i, 4) * TAU;
        let radius = noise(i, 5).sqrt();
        let seen = egui::vec2(
            angle.cos() * radius * STAGE.x * 0.47,
            angle.sin() * radius * STAGE.y * 0.45,
        );
        let start = seen / k0;

        let depart = FIRST_DEPARTURE + target.order * SPREAD + (noise(i, 6) - 0.5) * 0.12;
        let p = ((t - depart) / FLIGHT).clamp(0.0, 1.0);
        let e = 1.0 - (1.0 - p).powi(3);

        // Bow the path sideways, most at mid-flight, so grains swirl in
        // rather than sliding along straight lines.
        let travel = landing - start;
        let side = egui::vec2(-travel.y, travel.x) / travel.length().max(1e-3);
        let bow = (noise(i, 7) - 0.5) * 36.0 * (PI * e).sin();
        let mut at = start + travel * e + side * bow;
        if p == 0.0 {
            at.y += (t * 0.9 + noise(i, 8) * TAU).sin() * 2.0;
        }
        let z = z0 * (1.0 - e);
        let k = 1.0 / (1.0 + PERSPECTIVE * z);
        let pos = centre + at * k;

        // Out of focus off the mark's plane: a big, soft, dim disc that
        // sharpens to a bright point as it lands.
        let defocus = (z / DEPTH_MAX).clamp(0.0, 1.0);
        let r = 0.6 + 3.4 * defocus;
        let twinkle = 0.85 + 0.15 * (t * 3.0 + noise(i, 9) * TAU).sin();
        let a = (0.95 - 0.7 * defocus) * twinkle * alpha;
        let landed_for = t - depart - FLIGHT;
        let flash = if landed_for < 0.0 {
            smoothstep(0.8, 1.0, p)
        } else {
            (-landed_for * 8.0).exp()
        };
        let color = mix(colors.accent, egui::Color32::WHITE, 0.75 * flash);
        painter.circle_filled(pos, r, color.gamma_multiply(a * 0.3));
        painter.circle_filled(pos, (r * 0.38).max(0.55), color.gamma_multiply(a));
    }
}

/// A short run of light along the ∞, its head at `head` (0 to 1 of the way
/// round) with a fading tail, brightest at `strength` 1.
fn draw_light_run(
    painter: &egui::Painter,
    layout: &QpMarkLayout,
    colors: &AppColors,
    head: f32,
    strength: f32,
) {
    const TAIL: f32 = 0.22;
    let path = qp_infinity_points();
    let (cum, total) = arc_lengths(&path);
    let n = path.len();
    // Run the head past the end so the tail leaves the loop too.
    let head = head * (1.0 + TAIL);
    for j in 0..n {
        let behind = head - (cum[j] + cum[j + 1]) * 0.5 / total;
        if !(0.0..TAIL).contains(&behind) {
            continue;
        }
        let f = 1.0 - behind / TAIL;
        let color = mix(colors.accent, egui::Color32::WHITE, f).gamma_multiply(f * f * strength);
        let (a, b) = (path[j], path[(j + 1) % n]);
        painter.line_segment(
            [layout.point(a.0, a.1), layout.point(b.0, b.1)],
            egui::Stroke::new(layout.unit * (0.5 + 0.5 * f), color),
        );
    }
}

/// Dust drifting left across the stage, nearer motes faster. The far layer
/// sits behind the mark; the near layer passes in front of it as large,
/// faint, out-of-focus discs.
fn draw_dust(
    painter: &egui::Painter,
    stage: egui::Rect,
    colors: &AppColors,
    t: f32,
    fade_in: f32,
    near: bool,
) {
    let (count, offset) = if near {
        (NEAR_DUST, 20_000)
    } else {
        (FAR_DUST, 10_000)
    };
    let span = stage.width() + 40.0;
    for j in 0..count {
        let i = offset + j;
        let z = if near {
            -0.55 + noise(i, 1) * 0.35
        } else {
            0.4 + noise(i, 1) * (DEPTH_MAX - 0.4)
        };
        let k = 1.0 / (1.0 + PERSPECTIVE * z);
        let x = (noise(i, 2) * span - 7.0 * k * t).rem_euclid(span) - 20.0;
        let y = noise(i, 3) * stage.height() + (t * 0.35 + noise(i, 4) * TAU).sin() * 3.0 * k;
        let pos = stage.left_top() + egui::vec2(x, y);
        let breathe = 0.8 + 0.2 * (t * 1.3 + noise(i, 5) * TAU).sin();
        if near {
            let r = 5.0 + noise(i, 6) * 4.0;
            painter.circle_filled(
                pos,
                r,
                colors.accent.gamma_multiply(0.06 * breathe * fade_in),
            );
        } else {
            let defocus = (z / DEPTH_MAX).clamp(0.0, 1.0);
            let r = 0.6 + 2.6 * defocus;
            let a = (0.10 + 0.22 * (1.0 - defocus)) * breathe * fade_in;
            painter.circle_filled(pos, r, colors.accent.gamma_multiply(a * 0.5));
            painter.circle_filled(pos, (r * 0.4).max(0.5), colors.accent.gamma_multiply(a));
        }
    }
}

/// Faint film grain over the stage, reseeded 24 times a second.
fn draw_film_grain(painter: &egui::Painter, stage: egui::Rect, t: f32) {
    let frame = (t * 24.0) as usize % 4096;
    for j in 0..90 {
        let i = 50_000 + frame * 97 + j;
        let pos = stage.left_top()
            + egui::vec2(noise(i, 1) * stage.width(), noise(i, 2) * stage.height());
        let a = 0.03 + noise(i, 3) * 0.05;
        painter.rect_filled(
            egui::Rect::from_min_size(pos, egui::vec2(1.0, 1.0)),
            0.0,
            egui::Color32::WHITE.gamma_multiply(a),
        );
    }
}

/// Cumulative arc length at each point of a closed path (one entry per
/// point, plus the closing segment), and the total.
fn arc_lengths(path: &[(f32, f32)]) -> (Vec<f32>, f32) {
    let n = path.len();
    let mut cum = Vec::with_capacity(n + 1);
    cum.push(0.0);
    for j in 0..n {
        let (a, b) = (path[j], path[(j + 1) % n]);
        cum.push(cum[j] + ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt());
    }
    let total = cum[n];
    (cum, total)
}

/// Deterministic noise in [0, 1) for item `i`, channel `k` (splitmix64), so
/// the dust falls the same way every time.
fn noise(i: usize, k: u64) -> f32 {
    let mut z = (((i as u64) << 8) | k).wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    (z >> 40) as f32 / (1u64 << 24) as f32
}

fn smoothstep(from: f32, to: f32, x: f32) -> f32 {
    let t = ((x - from) / (to - from)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Blend two opaque colours, `t` = 0 giving `a`.
fn mix(a: egui::Color32, b: egui::Color32, t: f32) -> egui::Color32 {
    let blend = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    egui::Color32::from_rgb(
        blend(a.r(), b.r()),
        blend(a.g(), b.g()),
        blend(a.b(), b.b()),
    )
}
