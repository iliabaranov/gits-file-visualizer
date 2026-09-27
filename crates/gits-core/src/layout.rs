//! Tower layout: every directory becomes a band of one or more rings (depth-first
//! order, `~` at the top); files are arc segments on those rings.

use crate::model::{DirId, FileId, Model};
use crate::rng::mix64;

pub const SEG_DIR: u32 = 1;
pub const SEG_AGG: u32 = 2;
pub const SEG_SYMLINK: u32 = 4;
pub const SEG_HIDDEN: u32 = 8;
pub const SEG_DELETED: u32 = 16;

pub const NONE: u32 = u32::MAX;

#[derive(Clone, Debug)]
pub struct LayoutParams {
    pub min_arc: f32,
    pub max_arc: f32,
    pub gap: f32,
    pub dir_marks: bool,
    pub dir_mark_arc: f32,
    pub max_rings: usize,
}

#[derive(Clone, Copy, Debug)]
pub struct Ring {
    pub dir: DirId,
    pub depth: u16,
    pub index_in_dir: u16,
}

impl Ring {
    /// Stable identity across relayouts (used to keep animation state).
    pub fn key(&self) -> u64 {
        ((self.dir as u64) << 16) | self.index_in_dir as u64
    }

    pub fn hash(&self) -> u64 {
        mix64(self.key() ^ 0x5EED_2501)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SegSource {
    File(FileId),
    Dir(DirId),
    /// A collapsed subtree rooted at this directory.
    Agg(DirId),
}

#[derive(Clone, Copy, Debug)]
pub struct Segment {
    pub source: SegSource,
    pub ring: u32,
    pub a0: f32,
    pub a1: f32,
    pub flags: u32,
}

#[derive(Clone, Debug, Default)]
pub struct Layout {
    pub rings: Vec<Ring>,
    pub segments: Vec<Segment>,
    /// Segment showing each file ([`NONE`] if not shown).
    pub seg_of_file: Vec<u32>,
    /// First ring of each directory's band ([`NONE`] if the directory is collapsed).
    pub dir_first_ring: Vec<u32>,
    /// Directory depth at and below which subtrees were collapsed, if any.
    pub collapse_depth: Option<u16>,
}

pub fn arc_for_size(size: u64, p: &LayoutParams) -> f32 {
    let x = ((size as f64 + 1.0).log2() / 34.0).clamp(0.0, 1.0) as f32;
    p.min_arc + (p.max_arc - p.min_arc) * x.powf(1.3)
}

/// Builds the layout, collapsing deep subtrees until it fits in `max_rings`.
pub fn build(model: &mut Model, p: &LayoutParams) -> Layout {
    for f in &mut model.files {
        if f.arc <= 0.0 {
            f.arc = arc_for_size(f.size, p);
        }
    }
    let max_depth = model.max_depth();
    let mut collapse: Option<u16> = None;
    loop {
        let l = build_with(model, p, collapse);
        if l.rings.len() <= p.max_rings.max(1) || collapse == Some(1) || max_depth <= 1 {
            return l;
        }
        collapse = Some(match collapse {
            None => max_depth.saturating_sub(1).max(1),
            Some(d) => (d - 1).max(1),
        });
    }
}

struct Placer<'a> {
    p: &'a LayoutParams,
    layout: Layout,
    cursor: f32,
    cur_dir: DirId,
    cur_depth: u16,
    ring_in_dir: u16,
    have_ring: bool,
}

impl Placer<'_> {
    fn begin_dir(&mut self, dir: DirId, depth: u16) {
        self.cur_dir = dir;
        self.cur_depth = depth;
        self.ring_in_dir = 0;
        self.have_ring = false;
        self.cursor = 0.0;
    }

    fn new_ring(&mut self) {
        if self.have_ring {
            self.ring_in_dir = self.ring_in_dir.saturating_add(1);
        }
        let idx = self.layout.rings.len() as u32;
        if self.ring_in_dir == 0 {
            self.layout.dir_first_ring[self.cur_dir as usize] = idx;
        }
        self.layout.rings.push(Ring { dir: self.cur_dir, depth: self.cur_depth, index_in_dir: self.ring_in_dir });
        self.have_ring = true;
        self.cursor = 0.0;
    }

    fn place(&mut self, source: SegSource, arc: f32, flags: u32) -> u32 {
        if !self.have_ring || (self.cursor > 0.0 && self.cursor + arc > 1.0 - 1e-4) {
            self.new_ring();
        }
        let g = self.p.gap * 0.5;
        let seg = Segment { source, ring: self.layout.rings.len() as u32 - 1, a0: self.cursor + g, a1: self.cursor + arc - g, flags };
        self.cursor += arc;
        self.layout.segments.push(seg);
        self.layout.segments.len() as u32 - 1
    }
}

fn build_with(model: &Model, p: &LayoutParams, collapse: Option<u16>) -> Layout {
    let subtree_size = subtree_sizes(model);
    let mut placer = Placer {
        p,
        layout: Layout {
            seg_of_file: vec![NONE; model.files.len()],
            dir_first_ring: vec![NONE; model.dirs.len()],
            collapse_depth: collapse,
            ..Default::default()
        },
        cursor: 0.0,
        cur_dir: model.root,
        cur_depth: 0,
        ring_in_dir: 0,
        have_ring: false,
    };

    let mut stack = vec![model.root];
    while let Some(d) = stack.pop() {
        let dir = &model.dirs[d as usize];
        placer.begin_dir(d, dir.depth);
        if p.dir_marks {
            placer.place(SegSource::Dir(d), p.dir_mark_arc, SEG_DIR);
        }
        for &f in &dir.files {
            let file = &model.files[f as usize];
            if file.purged {
                continue;
            }
            let mut flags = 0;
            if file.hidden {
                flags |= SEG_HIDDEN;
            }
            if file.symlink {
                flags |= SEG_SYMLINK;
            }
            let s = placer.place(SegSource::File(f), file.arc.max(p.min_arc), flags);
            placer.layout.seg_of_file[f as usize] = s;
        }
        if collapse.is_some_and(|c| dir.depth >= c) {
            for &c in &dir.dirs {
                let size = subtree_size[c as usize];
                let s = placer.place(SegSource::Agg(c), arc_for_size(size, p), SEG_AGG);
                assign_subtree(model, c, s, &mut placer.layout.seg_of_file);
            }
        } else {
            for &c in dir.dirs.iter().rev() {
                stack.push(c);
            }
        }
    }
    placer.layout
}

fn assign_subtree(model: &Model, d: DirId, seg: u32, seg_of_file: &mut [u32]) {
    let mut stack = vec![d];
    while let Some(d) = stack.pop() {
        let dir = &model.dirs[d as usize];
        for &f in &dir.files {
            seg_of_file[f as usize] = seg;
        }
        stack.extend_from_slice(&dir.dirs);
    }
}

fn subtree_sizes(model: &Model) -> Vec<u64> {
    let mut sizes: Vec<u64> = model.dirs.iter().map(|d| d.files.iter().map(|&f| model.files[f as usize].size).sum()).collect();
    // Children always have larger ids than parents, so a reverse sweep sums bottom-up.
    for d in (0..model.dirs.len()).rev() {
        if let Some(parent) = model.dirs[d].parent {
            sizes[parent as usize] += sizes[d];
        }
    }
    sizes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(max_rings: usize) -> LayoutParams {
        LayoutParams { min_arc: 0.02, max_arc: 0.2, gap: 0.006, dir_marks: true, dir_mark_arc: 0.03, max_rings }
    }

    fn sample() -> Model {
        let mut m = Model::new("~", 0);
        let a = m.add_dir(0, "a");
        let b = m.add_dir(a, "b");
        for i in 0..40 {
            m.add_existing_file(a, format!("f{i}.rs"), 1000 * i, 0);
            m.add_existing_file(b, format!("g{i}.png"), 2_000_000, 0);
        }
        m
    }

    #[test]
    fn segments_fit_rings_and_do_not_overlap() {
        let mut m = sample();
        let l = build(&mut m, &params(10_000));
        assert!(l.segments.iter().all(|s| s.a0 >= 0.0 && s.a1 <= 1.0 && s.a0 < s.a1));
        for w in l.segments.windows(2) {
            if w[0].ring == w[1].ring {
                assert!(w[0].a1 <= w[1].a0);
            }
        }
        assert!(l.seg_of_file.iter().all(|&s| s != NONE));
    }

    #[test]
    fn collapses_to_fit_budget() {
        let mut m = sample();
        let full = build(&mut m, &params(10_000)).rings.len();
        let l = build(&mut m, &params(full - 1));
        assert!(l.collapse_depth.is_some());
        assert!(l.segments.iter().any(|s| s.flags & SEG_AGG != 0));
        assert!(l.seg_of_file.iter().all(|&s| s != NONE));
    }
}
