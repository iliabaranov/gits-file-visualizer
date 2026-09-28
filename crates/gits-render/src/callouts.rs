//! Callouts: labels naming the files being read, edited, created and deleted.
//!
//! Labels sit in two tidy columns beside the tower, with a leader line to the
//! file (like the outside labels of a pie chart), so several dozen stay
//! readable at once. Each monitor places them itself ([`Slots`]): a label is
//! given a free row near its file's height and keeps it until it fades, so
//! labels never slide around; only the leader line follows the turning ring.

use crate::camera::Camera;
use crate::fx::{human_size, line_px, quad_world, text_px};
use crate::gpu_types::{SpriteVertex, sprite_kind as K};
use crate::text::GlyphAtlas;
use crate::world::{World, smoothstep};
use gits_core::config::Labels;
use gits_core::model::{EventKind, FileId, Model};
use glam::Vec3;
use std::collections::HashMap;

/// Fade-in and fade-out times (seconds).
const FADE_IN: f64 = 0.4;
const FADE_OUT: f64 = 1.5;
/// Repeated events keep a callout up for at most this many `hold`s.
const MAX_LIFE_HOLDS: f64 = 3.0;
/// New callouts may come in short bursts of this many.
const BURST: f32 = 4.0;
/// Leader lines ease toward their file with this time constant (seconds), so a
/// new file's line glides from its folder to the file when the file appears.
const ANCHOR_TAU: f32 = 0.3;

#[derive(Clone, Debug)]
struct Card {
    id: u64,
    file: FileId,
    kind: EventKind,
    t0: f64,
    t_last: f64,
    /// Set when the card is pushed out early to make room.
    fade_from: Option<f64>,
}

/// Most callouts kept at once, including those fading out (which still hold
/// their rows).
fn cap(max: usize) -> usize {
    max + max / 2 + 1
}

impl Card {
    fn t_end(&self, hold: f64) -> f64 {
        self.fade_from.map_or(self.t_last + hold, |f| f.min(self.t_last + hold)) + FADE_OUT
    }

    fn active(&self, t: f64, hold: f64) -> bool {
        self.fade_from.is_none() && t < self.t_last + hold
    }
}

#[derive(Default)]
pub struct Callouts {
    cards: Vec<Card>,
    /// When each file last got a callout: files touched constantly (logs,
    /// browser profiles) do not keep one pinned.
    carded: HashMap<FileId, f64>,
    next_id: u64,
    tokens: f32,
    t_tokens: f64,
}

impl Callouts {
    pub fn clear(&mut self) {
        self.cards.clear();
    }

    pub fn on_event(&mut self, cfg: &Labels, kind: EventKind, file: FileId, t: f64) {
        if cfg.max == 0 {
            return;
        }
        let hold = cfg.hold.0 as f64;
        if let Some(c) = self.cards.iter_mut().find(|c| c.file == file) {
            c.kind = match (c.kind, kind) {
                (_, EventKind::Delete) | (EventKind::Delete, _) => EventKind::Delete,
                (EventKind::Create, _) => EventKind::Create,
                (EventKind::Write, EventKind::Read) => EventKind::Write,
                (_, k) => k,
            };
            if c.fade_from.is_none() && t - c.t0 < hold * MAX_LIFE_HOLDS {
                c.t_last = t;
            }
            return;
        }
        if kind == EventKind::Read && !cfg.reads {
            return;
        }
        // Creates and deletes happen once per file; reads and edits repeat.
        let once = matches!(kind, EventKind::Create | EventKind::Delete);
        if !once && self.carded.get(&file).is_some_and(|&t0| t - t0 < cfg.repeat.0 as f64) {
            return;
        }
        self.tokens = (self.tokens + (t - self.t_tokens) as f32 * cfg.per_second.max(0.0)).min(BURST);
        self.t_tokens = t;
        // Reads leave one token spare for the other kinds.
        let need = if kind == EventKind::Read { 2.0 } else { 1.0 };
        if self.tokens < need {
            return;
        }
        if self.cards.len() >= cap(cfg.max) {
            return;
        }
        let active = self.cards.iter().filter(|c| c.active(t, hold)).count();
        if active >= cfg.max {
            // Make room: the oldest active callout starts fading now.
            if let Some(c) = self.cards.iter_mut().filter(|c| c.active(t, hold)).min_by(|a, b| a.t_last.total_cmp(&b.t_last)) {
                c.fade_from = Some(t);
            }
        }
        self.tokens -= 1.0;
        self.carded.insert(file, t);
        if self.carded.len() > 4096 {
            let repeat = cfg.repeat.0 as f64;
            self.carded.retain(|_, t0| t - *t0 < repeat);
        }
        self.next_id += 1;
        self.cards.push(Card { id: self.next_id, file, kind, t0: t, t_last: t, fade_from: None });
    }

    pub fn step(&mut self, cfg: &Labels, model: &Model, t: f64) {
        let hold = cfg.hold.0 as f64;
        self.cards.retain(|c| t < c.t_end(hold) && model.files.get(c.file as usize).is_some_and(|f| !f.purged));
        if self.cards.len() > cap(cfg.max) {
            // `max` was lowered: drop the oldest.
            let excess = self.cards.len() - cap(cfg.max);
            self.cards.drain(..excess);
        }
    }

    /// Callout sprites for one monitor.
    #[allow(clippy::too_many_arguments)]
    pub fn build(
        &self,
        w: &World,
        cam: &Camera,
        atlas: &GlyphAtlas,
        slots: &mut Slots,
        width: f32,
        height: f32,
        dt: f32,
        out: &mut Vec<SpriteVertex>,
    ) {
        let cfg = &w.cfg;
        let t = w.t;
        let hold = cfg.labels.hold.0 as f64;
        let ui = width.min(height) / 1080.0;
        let face = &atlas.mono;
        let (name_px, sub_px) = (13.0 * ui, 10.5 * ui);
        let pitch = 31.0 * ui;
        let (top, bottom) = (44.0 * ui, height - 56.0 * ui);
        let rows = ((bottom - top) / pitch).floor().max(1.0) as usize;
        let margin = 20.0 * ui;
        slots.reset_if(width, height, rows);
        slots.forget_missing(&self.cards);
        if self.cards.is_empty() {
            return;
        }

        // Label columns just outside the tower's silhouette (over it when the
        // tower fills the screen).
        let (tl, tr) = tower_extent(w, cam, width, height);
        let (min_w, gap) = (160.0 * ui, 34.0 * ui);
        let left_x = (tl - gap).clamp(margin + min_w, (width * 0.5 - gap).max(margin + min_w));
        let right_x = (tr + gap).clamp((width * 0.5 + gap).min(width - margin - min_w), width - margin - min_w);
        let max_w = [(left_x - margin).min(300.0 * ui), (width - margin - right_x).min(300.0 * ui)];

        // Place new callouts: the free row nearest their file, on its side of the axis.
        for c in &self.cards {
            if slots.placed.contains_key(&c.id) || !c.active(t, hold) {
                continue;
            }
            let Some((p, _)) = w.anchor_segment(c.file).and_then(|s| w.segment_point(s, 0.5)) else { continue };
            let Some((sx, sy)) = cam.project(p, width, height) else { continue };
            if !(0.0..width).contains(&sx) || !(0.0..height).contains(&sy) {
                continue;
            }
            let axis_x = cam.project(Vec3::new(0.0, p.y, 0.0), width, height).map_or(width * 0.5, |a| a.0);
            let side = usize::from(sx >= axis_x);
            let want = (((sy - top) / pitch - 0.5).round().max(0.0) as usize).min(rows - 1);
            if let Some(row) =
                slots.free_row(side, want).map(|r| (side, r)).or_else(|| slots.free_row(1 - side, want).map(|r| (1 - side, r)))
            {
                slots.take(c.id, row.0, row.1, t, [sx, sy]);
            }
        }

        let gold = cfg.color(&cfg.age.fresh);
        let cream = cfg.color(&cfg.events.read.color);
        let k = 1.0 - (-dt.max(0.0) / ANCHOR_TAU).exp();
        let paths = w.label_mode() == "paths";
        for c in &self.cards {
            let Some(pl) = slots.placed.get_mut(&c.id) else { continue };
            let t_end = c.t_end(hold);
            let alpha = (smoothstep(0.0, FADE_IN as f32, (t - pl.t) as f32)
                * (1.0 - smoothstep((t_end - FADE_OUT) as f32, t_end as f32, t as f32)))
            .max(0.0);
            if alpha <= 0.002 {
                continue;
            }
            let anchor = w.anchor_segment(c.file).and_then(|s| w.segment_point(s, 0.5));
            let mut vis = 1.0;
            if let Some((p, r)) = anchor {
                let normal = Vec3::new(p.x, 0.0, p.z) / r.max(1e-3);
                let facing = normal.dot((cam.eye - p).normalize());
                vis = 0.35 + 0.65 * smoothstep(-0.2, 0.3, facing);
                if let Some((sx, sy)) = cam.project(p, width, height) {
                    pl.anchor[0] += (sx - pl.anchor[0]) * k;
                    pl.anchor[1] += (sy - pl.anchor[1]) * k;
                }
                // The film's white card on the touched segment.
                let tangent = Vec3::new(-normal.z, 0.0, normal.x);
                let center = p + normal * 0.03;
                let (hw, hh) = (0.035, 0.13);
                quad_world(
                    out,
                    [
                        center - tangent * hw - Vec3::Y * hh,
                        center + tangent * hw - Vec3::Y * hh,
                        center + tangent * hw + Vec3::Y * hh,
                        center - tangent * hw + Vec3::Y * hh,
                    ],
                    K::CARD,
                    [cream[0], cream[1], cream[2], 1.3 * alpha * vis],
                );
            }
            let Some(name) = w.label_for(c.file) else { continue };
            let (tag, tag_col) = match c.kind {
                EventKind::Create => ("NEW", cfg.color(&cfg.events.create.color)),
                EventKind::Write => ("EDIT", cfg.color(&cfg.events.write.color)),
                EventKind::Read => ("READ", cream),
                EventKind::Delete => ("DELETED", cfg.color(&cfg.events.delete.color)),
            };
            let file = &w.model.files[c.file as usize];
            let mut rest = format!("  {}", human_size(file.size));
            let (side, row) = (pl.side, pl.row);
            let cw = face.measure("0", sub_px).max(1e-3);
            let max_chars = (max_w[side] / cw) as usize;
            if paths {
                let room = max_chars.saturating_sub(tag.len() + rest.chars().count() + 2);
                if room >= 6 {
                    rest = format!("{rest}  {}", fit_left(&w.model.dir_path(file.dir), room));
                }
            }
            let name = fit_right(&name, (max_w[side] / face.measure("0", name_px).max(1e-3)) as usize);
            let name_w = face.measure(&name, name_px);
            let sub_w = face.measure(tag, sub_px) + face.measure(&rest, sub_px);
            let y = top + (row as f32 + 0.5) * pitch;
            let (name_y, sub_y) = (y - 1.0 * ui, y + 12.0 * ui);
            let (lx_name, lx_sub, edge, knee) = if side == 0 {
                (left_x - name_w, left_x - sub_w, left_x + 5.0 * ui, left_x + 18.0 * ui)
            } else {
                (right_x, right_x, right_x - 5.0 * ui, right_x - 18.0 * ui)
            };
            let a = alpha * vis;
            let line_y = name_y - name_px * 0.3;
            let line_col = [gold[0], gold[1], gold[2], 0.55 * a];
            line_px(out, pl.anchor, [knee, line_y], 1.0 * ui, line_col, width, height);
            line_px(out, [knee, line_y], [edge, line_y], 1.0 * ui, line_col, width, height);
            text_px(out, face, &name, lx_name, name_y, name_px, [gold[0], gold[1], gold[2], 1.5 * a], width, height);
            text_px(out, face, tag, lx_sub, sub_y, sub_px, [tag_col[0], tag_col[1], tag_col[2], 1.4 * a], width, height);
            let tag_w = face.measure(tag, sub_px);
            text_px(out, face, &rest, lx_sub + tag_w, sub_y, sub_px, [gold[0], gold[1], gold[2], 0.75 * a], width, height);
        }
    }
}

/// Horizontal pixel extent of the visible part of the tower on screen.
fn tower_extent(w: &World, cam: &Camera, width: f32, height: f32) -> (f32, f32) {
    let r = w.cfg.tower.radius;
    let (y0, h) = (w.y_top(), w.cfg.tower.height);
    let (mut lo, mut hi) = (f32::MAX, f32::MIN);
    for j in 0..=8 {
        let y = y0 - h * j as f32 / 8.0;
        for i in 0..32 {
            let a = std::f32::consts::TAU * i as f32 / 32.0;
            if let Some((sx, sy)) = cam.project(Vec3::new(r * a.cos(), y, r * a.sin()), width, height)
                && (0.0..height).contains(&sy)
            {
                lo = lo.min(sx);
                hi = hi.max(sx);
            }
        }
    }
    if lo > hi { (width * 0.4, width * 0.6) } else { (lo, hi) }
}

/// `s` shortened to `n` characters, keeping its start.
fn fit_right(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    let keep: String = s.chars().take(n.saturating_sub(2)).collect();
    format!("{keep}..")
}

/// `s` shortened to `n` characters, keeping its end.
fn fit_left(s: &str, n: usize) -> String {
    let len = s.chars().count();
    if len <= n {
        return s.to_string();
    }
    let keep: String = s.chars().skip(len - n.saturating_sub(2)).collect();
    format!("..{keep}")
}

struct Placed {
    side: usize,
    row: usize,
    /// When this monitor first showed the callout.
    t: f64,
    /// Smoothed screen position of the file.
    anchor: [f32; 2],
}

/// Where one monitor shows each callout: a side (0 = left, 1 = right) and a row.
#[derive(Default)]
pub struct Slots {
    size: (u32, u32),
    rows: [Vec<Option<u64>>; 2],
    placed: HashMap<u64, Placed>,
}

impl Slots {
    fn reset_if(&mut self, width: f32, height: f32, rows: usize) {
        let size = (width as u32, height as u32);
        if self.size != size || self.rows[0].len() != rows {
            self.size = size;
            self.rows = [vec![None; rows], vec![None; rows]];
            self.placed.clear();
        }
    }

    fn forget_missing(&mut self, cards: &[Card]) {
        if self.placed.len() == cards.len() && cards.iter().all(|c| self.placed.contains_key(&c.id)) {
            return;
        }
        self.placed.retain(|id, _| cards.iter().any(|c| c.id == *id));
        for side in &mut self.rows {
            for r in side.iter_mut() {
                if r.is_some_and(|id| !self.placed.contains_key(&id)) {
                    *r = None;
                }
            }
        }
    }

    fn free_row(&self, side: usize, want: usize) -> Option<usize> {
        let rows = &self.rows[side];
        (0..rows.len()).flat_map(|d| [want.checked_sub(d), Some(want + d)]).flatten().find(|&r| r < rows.len() && rows[r].is_none())
    }

    fn take(&mut self, id: u64, side: usize, row: usize, t: f64, anchor: [f32; 2]) {
        self.rows[side][row] = Some(id);
        self.placed.insert(id, Placed { side, row, t, anchor });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortening() {
        assert_eq!(fit_right("report-final.pdf", 40), "report-final.pdf");
        assert_eq!(fit_right("report-final.pdf", 8), "report..");
        assert_eq!(fit_left("~/a/b/c/deep", 8), "..c/deep");
    }

    #[test]
    fn nearest_free_row() {
        let mut s = Slots::default();
        s.reset_if(100.0, 100.0, 5);
        s.take(1, 0, 2, 0.0, [0.0; 2]);
        s.take(2, 0, 1, 0.0, [0.0; 2]);
        assert_eq!(s.free_row(0, 2), Some(3));
        assert_eq!(s.free_row(0, 1), Some(0));
        assert_eq!(s.free_row(1, 2), Some(2));
    }

    #[test]
    fn rate_and_capacity() {
        let cfg =
            Labels { max: 3, hold: gits_core::config::Dur(8.0), repeat: gits_core::config::Dur(30.0), per_second: 100.0, reads: true };
        let mut m = Model::new("~", 0);
        let files: Vec<FileId> = (0..12).map(|i| m.add_existing_file(m.root, format!("f{i}"), 10, 0)).collect();
        let mut c = Callouts::default();
        for (i, &f) in files.iter().enumerate().take(10) {
            let t = 1.0 + i as f64;
            c.step(&cfg, &m, t);
            c.on_event(&cfg, EventKind::Write, f, t);
        }
        let active: Vec<FileId> = c.cards.iter().filter(|x| x.active(10.0, 8.0)).map(|x| x.file).collect();
        assert_eq!(active, files[7..10]);
        assert!(c.cards.len() <= 5);
        // An edit to a file that recently had a callout does not bring it back...
        c.step(&cfg, &m, 30.0);
        assert!(c.cards.is_empty());
        c.on_event(&cfg, EventKind::Write, files[9], 30.0);
        assert!(c.cards.is_empty());
        // ...but deleting it does.
        c.on_event(&cfg, EventKind::Delete, files[9], 30.5);
        assert_eq!(c.cards.len(), 1);
        // Two new callouts may not start in the same instant beyond the burst.
        for &f in &files[10..] {
            c.on_event(&cfg, EventKind::Read, f, 30.5);
        }
        assert_eq!(c.cards.len(), 3);
    }
}
