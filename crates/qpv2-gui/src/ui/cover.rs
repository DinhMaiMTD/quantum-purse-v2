//! The cover's mark: q and p, floating apart, that join into the qp∞ logo
//! as the user holds to unlock.
//!
//! Each letter carries half of the ∞'s crossing: q its ring closing to a
//! point at the crossing (a ">"), p its ring closing to the same point (a
//! "<"). When the cover appears the joined mark splits and the two drift
//! apart, dimmed. While the hold button is held they glide together,
//! accelerating like magnets, their drift and tilt settling and their facing
//! tips glowing, drawing in and brightening just before they touch; released
//! early, they ease back apart. When the hold completes they meet in a burst
//! of light, the weave cuts in with a flash on "\", a shockwave of light
//! sweeps out across the window and a light runs once round the ∞; then the
//! wallet opens.
//! Everything is strokes, fills and meshes. egui has no blur, so a glow is a
//! mesh whose colour fades smoothly to nothing at its edge, and it adds light
//! to what lies under it rather than covering it.

use std::f32::consts::{PI, TAU};

use eframe::egui;
use egui::{Color32, Pos2, Rect, Stroke, Vec2};

use super::utils::{
    draw_qp_logo, qp_infinity_points, qp_mark_parts, Hold, QpMarkLayout, QP_STEM_BOTTOM,
};
use crate::types::AppColors;

/// The mark's square, as `QpMarkLayout` places it.
const MARK_SIZE: f32 = 128.0;
/// The stage the letters float in.
const STAGE: Vec2 = egui::vec2(420.0, 210.0);
/// How far each letter floats out from the joined mark, in points.
const APART: f32 = 70.0;
/// Joined to apart when the cover appears.
const SPLIT_SECS: f32 = 0.7;
/// From the hold completing to the wallet opening.
const FINALE_SECS: f32 = 0.95;
/// The letters' brightness while apart, 0 to 1 of full colour.
const DIM: f32 = 0.55;
/// How quickly the letters ease back apart after an early release.
const RELEASE_EASE: f32 = 0.12;

/// Reserves the stage in the layout. `draw_cover` paints it once the hold
/// button below has reported this frame's progress.
pub(crate) fn reserve_stage(ui: &mut egui::Ui) -> Rect {
    let width = ui.available_width().min(STAGE.x);
    ui.allocate_exact_size(egui::vec2(width, STAGE.y), egui::Sense::hover())
        .0
}

/// Where the letters end at rest, for laying out what sits below them.
pub(crate) fn letters_bottom(stage: Rect) -> f32 {
    let layout = QpMarkLayout::new(mark_square(stage));
    layout.centre.y + QP_STEM_BOTTOM * layout.unit
}

/// The mark's square, centred in the stage.
fn mark_square(stage: Rect) -> Rect {
    Rect::from_center_size(stage.center(), egui::vec2(MARK_SIZE, MARK_SIZE))
}

/// What `draw_cover` reports for this frame.
pub(crate) struct CoverFrame {
    /// How far the letters have closed, 0 apart to 1 joined: the hold's
    /// progress, eased back on an early release and held at 1 while the
    /// joining plays.
    pub(crate) pull: f32,
    /// True once the hold has completed and the joining has played out:
    /// the moment to open the wallet.
    pub(crate) done: bool,
}

/// Paints the stage for this frame's `hold`.
pub(crate) fn draw_cover(
    ui: &egui::Ui,
    stage: Rect,
    colors: &AppColors,
    hold: &Hold,
) -> CoverFrame {
    let now = ui.input(|i| i.time);
    let dt = ui.input(|i| i.stable_dt).min(0.05);
    let mut state = ui
        .ctx()
        .memory(|m| m.data.get_temp::<State>(state_id()))
        .unwrap_or(State {
            appeared_at: now,
            pull: 0.0,
            finale_at: None,
        });

    if state.finale_at.is_none() {
        if hold.fired {
            state.finale_at = Some(now);
            state.pull = 1.0;
        } else {
            // Ease back apart on release, never snap. A fresh hold starts
            // from zero; until it catches up, the letters keep easing.
            let easing = state.pull * (-dt / RELEASE_EASE).exp();
            state.pull = hold.progress.max(easing);
        }
    }
    ui.ctx()
        .memory_mut(|m| m.data.insert_temp(state_id(), state));

    let painter = ui.painter_at(stage);
    let square = mark_square(stage);
    let done = match state.finale_at {
        Some(at) => {
            let f = (now - at) as f32;
            // The ripple crosses the whole window, so it is clipped to the
            // window rather than the stage, on the same layer as the rest.
            let window = ui.ctx().layer_painter(ui.layer_id());
            draw_finale(&painter, &window, square, colors, f);
            ui.ctx().request_repaint();
            f >= FINALE_SECS
        }
        None => {
            let t = (now - state.appeared_at) as f32;
            draw_letters(&painter, square, colors, t, state.pull);
            if t < SPLIT_SECS || state.pull > 0.001 {
                ui.ctx().request_repaint();
            } else {
                // Only the slow float is moving; 30 fps is smooth for it.
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(33));
            }
            false
        }
    };
    CoverFrame {
        pull: state.pull,
        done,
    }
}

/// Splits the mark apart again the next time the cover is drawn.
pub(crate) fn restart_cover(ctx: &egui::Context) {
    ctx.memory_mut(|m| m.data.remove::<State>(state_id()));
}

fn state_id() -> egui::Id {
    egui::Id::new("cover-state")
}

/// The cover's animation state, kept across frames in egui's memory.
#[derive(Clone, Copy)]
struct State {
    /// When the cover appeared, for the split.
    appeared_at: f64,
    /// How far the letters have closed, 0 apart to 1 joined.
    pull: f32,
    /// When the hold completed, while the joining plays out.
    finale_at: Option<f64>,
}

/// One letter's slow float: side (-1 for q, 1 for p), bob and sway in
/// points, tilt in degrees. Each runs on its own periods, so the two never
/// move in step.
struct Float {
    side: f32,
    bob: f32,
    sway: f32,
    tilt: f32,
}

fn floats(t: f32) -> [Float; 2] {
    let wave = |period: f32, phase: f32| (t * TAU / period + phase).sin();
    [
        Float {
            side: -1.0,
            bob: 4.0 * wave(3.7, 0.0),
            sway: 2.0 * wave(6.1, 0.0),
            tilt: 2.6 * wave(5.3, 1.0),
        },
        Float {
            side: 1.0,
            bob: 4.0 * wave(4.3, 2.0),
            sway: 2.0 * wave(5.7, 1.0),
            tilt: 2.6 * wave(4.9, 3.0),
        },
    ]
}

/// Where a letter sits: shifted, then tilted about a point in its bowl.
struct Placement {
    centre: Pos2,
    unit: f32,
    pivot: Vec2,
    cos: f32,
    sin: f32,
}

impl Placement {
    /// A point given in stroke widths from the joined mark's crossing.
    fn point(&self, (x, y): (f32, f32)) -> Pos2 {
        let d = egui::vec2(x, y) - self.pivot;
        let turned = egui::vec2(
            d.x * self.cos - d.y * self.sin,
            d.x * self.sin + d.y * self.cos,
        );
        self.centre + (turned + self.pivot) * self.unit
    }
}

fn draw_letters(painter: &egui::Painter, square: Rect, colors: &AppColors, t: f32, pull: f32) {
    let layout = QpMarkLayout::new(square);
    let appear = ease_out_back((t / SPLIT_SECS).clamp(0.0, 1.0));
    // Magnets: the pull accelerates into contact.
    let apart = appear * (1.0 - pull * pull);
    let color = mix(colors.bg, colors.accent, 1.0 - (1.0 - DIM) * apart);
    let tip_glow = smoothstep(0.35, 1.0, pull);
    // Just before contact the glow draws in and brightens, so the hit has
    // something to release.
    let inhale = smoothstep(0.8, 1.0, pull);

    let path = qp_infinity_points();
    let half = path.len() / 2;
    let parts = qp_mark_parts(layout.unit, painter.pixels_per_point());
    let placements = floats(t).map(|f| {
        // Drift and tilt settle as the letters close.
        let angle = (f.tilt * apart).to_radians();
        Placement {
            centre: layout.centre
                + egui::vec2(f.side * APART * apart + f.sway * apart, f.bob * apart),
            unit: layout.unit,
            pivot: egui::vec2(f.side * 2.4, 0.6),
            cos: angle.cos(),
            sin: angle.sin(),
        }
    });

    // q is the left ring and stem; p the right ones. The stems run the full
    // height here, unsplit, since nothing crosses them yet.
    let rings = [&path[..half], &path[half..]];
    let stems = [
        (parts.stems[0], parts.stems[2]),
        (parts.stems[1], parts.stems[3]),
    ];
    for ((place, ring), (upper, lower)) in placements.iter().zip(rings).zip(stems) {
        let mut outline: Vec<Pos2> = ring.iter().map(|&p| place.point(p)).collect();
        outline.push(place.point((0.0, 0.0)));
        painter.add(egui::Shape::closed_line(
            outline,
            Stroke::new(layout.unit, color),
        ));
        let stem = [upper[0], upper[1], lower[2], lower[3]].map(|p| place.point(p));
        painter.add(egui::Shape::convex_polygon(
            stem.to_vec(),
            color,
            Stroke::NONE,
        ));
    }

    // The facing tips glow as the gap closes. Over the letters, so the tips
    // themselves heat up, whiter as they are about to touch.
    if tip_glow > 0.0 {
        for place in &placements {
            glow(
                painter,
                place.point((0.0, 0.0)),
                (8.0 + 14.0 * tip_glow) * (1.0 - 0.45 * inhale),
                0.3 * tip_glow + 0.5 * inhale,
                mix(colors.accent, Color32::WHITE, 0.6 * inhale),
            );
        }
    }
}

/// The joining: a burst of light where the letters meet and a shockwave of
/// light sweeping out across the window, the mark settling with a small bump
/// and a flash on "\" as the weave cuts in, and a light running once round
/// the ∞.
fn draw_finale(
    painter: &egui::Painter,
    window: &egui::Painter,
    square: Rect,
    colors: &AppColors,
    f: f32,
) {
    let crossing = QpMarkLayout::new(square).centre;
    let area = window.clip_rect();

    // The hit: the whole window brightens for a frame or two.
    let wash = 1.0 - f / 0.05;
    if wash > 0.0 {
        window.rect_filled(area, 0.0, light(colors.accent, 0.1 * wash));
    }

    // Out to the window's farthest corner, so the shockwave sweeps the whole
    // window. It fades as it goes and is gone by the time it gets there.
    let reach = [
        area.left_top(),
        area.right_top(),
        area.left_bottom(),
        area.right_bottom(),
    ]
    .map(|corner| corner.distance(crossing))
    .into_iter()
    .fold(0.0, f32::max);
    let t = f / 0.8;
    if t < 1.0 {
        // Fast out and slowing as it spreads, thinner and dimmer as it grows.
        let radius = 12.0 + (reach - 12.0) * ease_out_cubic(t);
        let strength = smoothstep(0.0, 0.06, t) * (1.0 - t).powi(4);
        // Never most of the radius, so it is a ring from the first frame
        // rather than a filled disc.
        let trail = (10.0 + 40.0 * (1.0 - t)).min(0.6 * radius);
        shockwave(window, crossing, radius, trail, strength, colors);
    }

    // Scale about the crossing, so the bump doesn't shift the mark.
    let size = MARK_SIZE * (1.0 + 0.035 * (PI * (f / 0.22).clamp(0.0, 1.0)).sin());
    let anchor = QpMarkLayout::new(Rect::from_min_size(Pos2::ZERO, egui::vec2(size, size))).centre;
    let rect = Rect::from_min_size(crossing - anchor.to_vec2(), egui::vec2(size, size));
    draw_qp_logo(painter, rect, colors.accent, colors.bg);

    let layout = QpMarkLayout::new(rect);
    let flash = (1.0 - f / 0.3).clamp(0.0, 1.0);
    if flash > 0.0 {
        let color = mix(colors.accent, Color32::WHITE, 0.85 * flash);
        painter.line_segment(
            [layout.point(-1.5, -1.5), layout.point(1.5, 1.5)],
            Stroke::new(layout.unit, color),
        );
    }

    // The burst where the letters met, over the logo so it flares white, and
    // wide enough to spill well past it.
    let burst = (f / 0.25).min(1.0);
    if burst < 1.0 {
        glow(
            window,
            crossing,
            40.0 + 100.0 * ease_out_cubic(burst),
            0.9 * (1.0 - burst).powi(2),
            mix(colors.accent, Color32::WHITE, 0.7),
        );
    }
    light_run(painter, &layout, colors, (f / 0.7).clamp(0.0, 1.0));
}

/// A short run of light along the ∞, its head `head` (0 to 1) of the way
/// round, with a fading tail.
fn light_run(painter: &egui::Painter, layout: &QpMarkLayout, colors: &AppColors, head: f32) {
    const TAIL: f32 = 0.24;
    let path = qp_infinity_points();
    let n = path.len();
    let mut along = Vec::with_capacity(n + 1);
    along.push(0.0);
    for j in 0..n {
        let (a, b) = (path[j], path[(j + 1) % n]);
        along.push(along[j] + ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt());
    }
    let total = along[n];
    // Run the head past the end so the tail leaves the loop too.
    let head = head * (1.0 + TAIL);
    for j in 0..n {
        let behind = head - (along[j] + along[j + 1]) / 2.0 / total;
        if !(0.0..TAIL).contains(&behind) {
            continue;
        }
        let f = 1.0 - behind / TAIL;
        let color = mix(colors.accent, Color32::WHITE, f).gamma_multiply(f * f);
        let (a, b) = (path[j], path[(j + 1) % n]);
        painter.line_segment(
            [layout.point(a.0, a.1), layout.point(b.0, b.1)],
            Stroke::new(layout.unit * (0.45 + 0.4 * f), color),
        );
    }
}

/// A soft glow: light added over a disc, `strength` (0 to 1) at the centre
/// and fading smoothly to nothing at the rim.
fn glow(painter: &egui::Painter, at: Pos2, radius: f32, strength: f32, color: Color32) {
    // Close to an exponential falloff, so the glow shows no edge.
    let rows = [
        (0.0, 1.0),
        (0.2, 0.6),
        (0.45, 0.25),
        (0.7, 0.08),
        (1.0, 0.0),
    ]
    .map(|(r, k)| (radius * r, light(color, strength * k)));
    radial_band(painter, at, &rows, 48);
}

/// A ring of light `radius` out from `at`: a crisp, hot front edge with a
/// glow trailing `trail` points back towards the centre.
fn shockwave(
    painter: &egui::Painter,
    at: Pos2,
    radius: f32,
    trail: f32,
    strength: f32,
    colors: &AppColors,
) {
    let hot = mix(colors.accent, Color32::WHITE, 0.6);
    let rows = [
        (radius - trail, Color32::TRANSPARENT),
        (radius - 0.45 * trail, light(colors.accent, 0.15 * strength)),
        (radius - 0.12 * trail, light(colors.accent, 0.45 * strength)),
        (radius, light(hot, strength)),
        // A point and a half of falloff stands in for anti-aliasing, which
        // egui doesn't apply to meshes.
        (radius + 1.5, Color32::TRANSPARENT),
    ];
    // Enough segments that the edge stays round across the whole window.
    radial_band(painter, at, &rows, 256);
}

/// Concentric rows of vertices round `centre`, each `(radius, colour)`,
/// joined into one mesh so the colour blends smoothly from row to row.
fn radial_band(painter: &egui::Painter, centre: Pos2, rows: &[(f32, Color32)], segments: u32) {
    let mut mesh = egui::Mesh::default();
    for &(radius, color) in rows {
        for i in 0..segments {
            let angle = TAU * i as f32 / segments as f32;
            mesh.colored_vertex(centre + radius.max(0.0) * Vec2::angled(angle), color);
        }
    }
    for row in 1..rows.len() as u32 {
        let (inner, outer) = ((row - 1) * segments, row * segments);
        for i in 0..segments {
            let next = (i + 1) % segments;
            mesh.add_triangle(inner + i, inner + next, outer + i);
            mesh.add_triangle(inner + next, outer + next, outer + i);
        }
    }
    painter.add(egui::Shape::mesh(mesh));
}

/// `color` as light added to what lies under it, at `strength` (0 to 1),
/// rather than painted over it.
fn light(color: Color32, strength: f32) -> Color32 {
    color.gamma_multiply(strength).additive()
}

fn smoothstep(from: f32, to: f32, x: f32) -> f32 {
    let t = ((x - from) / (to - from)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn ease_out_cubic(t: f32) -> f32 {
    1.0 - (1.0 - t).powi(3)
}

/// Eases out past the target and settles back, for the split's overshoot.
fn ease_out_back(t: f32) -> f32 {
    let s = 1.3;
    let u = t - 1.0;
    1.0 + (s + 1.0) * u * u * u + s * u * u
}

/// Blend two opaque colours, `t` = 0 giving `a`.
fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let blend = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(
        blend(a.r(), b.r()),
        blend(a.g(), b.g()),
        blend(a.b(), b.b()),
    )
}
