//! Tower layout: every expanded directory becomes a band of one or more rings
//! (depth-first order, `~` at the top); files are arc segments on those rings.
//!
//! Large trees do not fit the ring budget, so directories are expanded greedily
//! by priority (current activity first, then how recently anything below them
//! changed) until the budget is used. A directory that is not expanded appears
//! as a single dashed "aggregate" segment in its parent's band, standing for
//! its whole subtree. No subtree may take more than `max_tree_rings`.

use crate::model::{DirId, FileId, Model};
use crate::rng::mix64;
use std::cmp::Ordering;
use std::collections::BinaryHeap;

pub const SEG_DIR: u32 = 1;
pub const SEG_AGG: u32 = 2;
pub const SEG_SYMLINK: u32 = 4;
pub const SEG_HIDDEN: u32 = 8;
pub const SEG_DELETED: u32 = 16;

pub const NONE: u32 = u32::MAX;

/// Ring estimates are inflated by this factor to account for packing waste.
const PACKING: f32 = 1.15;

#[derive(Clone, Debug)]
pub struct LayoutParams {
    pub min_arc: f32,
    pub max_arc: f32,
    pub gap: f32,
    pub dir_marks: bool,
    pub dir_mark_arc: f32,
    pub max_rings: usize,
    /// Most rings one directory's subtree may take (0 = no limit), so a single
    /// deep tree (a build tree, a browser profile) cannot fill the tower.
    pub max_tree_rings: usize,
}

#[derive(Clone, Copy, Debug)]
pub struct Ring {
    pub dir: DirId,
    pub depth: u16,
    pub index_in_dir: u16,
    /// The directory's path-derived key (see `Dir::key`).
    pub dir_key: u64,
}

impl Ring {
    /// Stable identity across relayouts and reconnects (keeps animation state).
    pub fn key(&self) -> u64 {
        mix64(self.dir_key ^ (self.index_in_dir as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15))
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
    /// For each collapsed directory directly below an expanded one: its aggregate segment.
    pub agg_seg: Vec<u32>,
    /// Number of directories expanded / collapsed into aggregates.
    pub expanded: usize,
    pub collapsed: usize,
}

impl Layout {
    pub fn is_expanded(&self, d: DirId) -> bool {
        self.dir_first_ring.get(d as usize).is_some_and(|&r| r != NONE)
    }

    /// The segment standing for directory `d`: none if `d` is expanded, otherwise
    /// the aggregate of its nearest collapsed ancestor-or-self.
    pub fn agg_for(&self, model: &Model, mut d: DirId) -> Option<u32> {
        loop {
            if self.is_expanded(d) {
                return None;
            }
            if let Some(&s) = self.agg_seg.get(d as usize).filter(|&&s| s != NONE) {
                return Some(s);
            }
            d = model.dirs.get(d as usize)?.parent?;
        }
    }
}

pub fn arc_for_size(size: u64, p: &LayoutParams) -> f32 {
    let x = ((size as f64 + 1.0).log2() / 34.0).clamp(0.0, 1.0) as f32;
    p.min_arc + (p.max_arc - p.min_arc) * x.powf(1.3)
}

#[derive(PartialEq)]
struct Cand {
    score: f64,
    dir: DirId,
}

impl Eq for Cand {}

impl PartialOrd for Cand {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

impl Ord for Cand {
    fn cmp(&self, o: &Self) -> Ordering {
        self.score.total_cmp(&o.score).then_with(|| o.dir.cmp(&self.dir))
    }
}

/// Gives files from index `from` on their arc length (frozen at first layout so
/// positions stay stable). Returns the number of files, to pass as `from` next time.
pub fn assign_arcs(model: &mut Model, p: &LayoutParams, from: usize) -> usize {
    for f in model.files.iter_mut().skip(from) {
        if f.arc <= 0.0 {
            f.arc = arc_for_size(f.size, p);
        }
    }
    model.files.len()
}

/// Builds the layout within `max_rings`. `hot` (per directory, may be shorter
/// than the directory list) boosts directories with current activity. Files
/// need their arcs assigned first (see [`assign_arcs`]).
pub fn build(model: &Model, p: &LayoutParams, hot: &[f32]) -> Layout {
    let expanded = choose_expanded(model, p, hot);
    place(model, p, &expanded)
}

fn choose_expanded(model: &Model, p: &LayoutParams, hot: &[f32]) -> Vec<bool> {
    let n = model.dirs.len();
    let mark = if p.dir_marks { p.dir_mark_arc } else { 0.0 };
    let mut own_arc = vec![0f32; n];
    let mut size = vec![0u64; n];
    let mut newest = vec![i64::MIN; n];
    let mut heat = vec![0f32; n];
    for (d, dir) in model.dirs.iter().enumerate() {
        if dir.purged {
            continue;
        }
        own_arc[d] = mark;
        for &f in &dir.files {
            let file = &model.files[f as usize];
            if file.purged {
                continue;
            }
            own_arc[d] += file.arc.max(p.min_arc);
            size[d] += file.size;
            newest[d] = newest[d].max(file.mtime);
        }
        heat[d] = hot.get(d).copied().unwrap_or(0.0);
    }
    // Children always have larger ids than parents: a reverse sweep aggregates bottom-up.
    for d in (1..n).rev() {
        if model.dirs[d].purged {
            continue;
        }
        if let Some(pa) = model.dirs[d].parent {
            let pa = pa as usize;
            size[pa] += size[d];
            newest[pa] = newest[pa].max(newest[d]);
            heat[pa] = heat[pa].max(heat[d]);
        }
    }
    let agg_arc = |d: usize| arc_for_size(size[d], p);
    // Band arc of `d` when expanded with all its children collapsed.
    let band_arc = |d: usize| -> f32 {
        own_arc[d] + model.dirs[d].dirs.iter().filter(|&&c| !model.dirs[c as usize].purged).map(|&c| agg_arc(c as usize)).sum::<f32>()
    };
    let rings_for = |arc: f32| -> usize { ((arc * PACKING).ceil() as usize).max(1) };
    let score = |d: usize| -> f64 {
        let recency = if newest[d] == i64::MIN { 0.0 } else { newest[d] as f64 };
        // Active subtrees first; then the most recently changed; top levels win ties.
        heat[d] as f64 * 1.0e12 + recency - model.dirs[d].depth as f64
    };

    let mut expanded = vec![false; n];
    let root = model.root as usize;
    expanded[root] = true;
    let mut arc_of = vec![0f32; n];
    arc_of[root] = band_arc(root);
    let mut total = rings_for(arc_of[root]);
    let budget = p.max_rings.max(1);
    let cap = if p.max_tree_rings > 0 { p.max_tree_rings as isize } else { isize::MAX };
    // Rings of each expanded directory's subtree.
    let mut tree = vec![0isize; n];
    let mut heap: BinaryHeap<Cand> = BinaryHeap::new();
    for &c in &model.dirs[root].dirs {
        if !model.dirs[c as usize].purged {
            heap.push(Cand { score: score(c as usize), dir: c });
        }
    }
    while let Some(Cand { dir, .. }) = heap.pop() {
        let c = dir as usize;
        let Some(pa) = model.dirs[c].parent.map(|x| x as usize) else { continue };
        let new_parent_arc = (arc_of[pa] - agg_arc(c)).max(0.0);
        let child_arc = band_arc(c);
        let delta = rings_for(child_arc) as isize + rings_for(new_parent_arc) as isize - rings_for(arc_of[pa]) as isize;
        // The top level is always shown in full.
        let top_level = model.dirs[c].depth <= 1;
        if total as isize + delta > budget as isize && !top_level {
            continue;
        }
        // Neither this subtree nor any enclosing one (below ~) may exceed the cap.
        let ancestors = || std::iter::successors(Some(pa), |&a| model.dirs[a].parent.map(|x| x as usize)).take_while(|&a| a != root);
        if !top_level && (rings_for(child_arc) as isize > cap || ancestors().any(|a| tree[a] + delta > cap)) {
            continue;
        }
        for a in ancestors() {
            tree[a] += delta;
        }
        tree[c] = rings_for(child_arc) as isize;
        expanded[c] = true;
        arc_of[pa] = new_parent_arc;
        arc_of[c] = child_arc;
        total = (total as isize + delta).max(0) as usize;
        for &g in &model.dirs[c].dirs {
            if !model.dirs[g as usize].purged {
                heap.push(Cand { score: score(g as usize), dir: g });
            }
        }
    }
    expanded
}

struct Placer<'a> {
    p: &'a LayoutParams,
    layout: Layout,
    cursor: f32,
    cur_dir: DirId,
    cur_depth: u16,
    cur_key: u64,
    ring_in_dir: u16,
    have_ring: bool,
}

impl Placer<'_> {
    fn begin_dir(&mut self, dir: DirId, depth: u16, key: u64) {
        self.cur_dir = dir;
        self.cur_depth = depth;
        self.cur_key = key;
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
        self.layout.rings.push(Ring { dir: self.cur_dir, depth: self.cur_depth, index_in_dir: self.ring_in_dir, dir_key: self.cur_key });
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

fn place(model: &Model, p: &LayoutParams, expanded: &[bool]) -> Layout {
    let sizes = subtree_sizes(model);
    let n = model.dirs.len();
    let mut placer = Placer {
        p,
        layout: Layout {
            seg_of_file: vec![NONE; model.files.len()],
            dir_first_ring: vec![NONE; n],
            agg_seg: vec![NONE; n],
            ..Default::default()
        },
        cursor: 0.0,
        cur_dir: model.root,
        cur_depth: 0,
        cur_key: 0,
        ring_in_dir: 0,
        have_ring: false,
    };

    let mut stack = vec![model.root];
    while let Some(d) = stack.pop() {
        let dir = &model.dirs[d as usize];
        placer.layout.expanded += 1;
        placer.begin_dir(d, dir.depth, dir.key);
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
        for &c in &dir.dirs {
            if model.dirs[c as usize].purged || expanded[c as usize] {
                continue;
            }
            let s = placer.place(SegSource::Agg(c), arc_for_size(sizes[c as usize], p), SEG_AGG);
            placer.layout.agg_seg[c as usize] = s;
            placer.layout.collapsed += 1;
            assign_subtree(model, c, s, &mut placer.layout.seg_of_file);
        }
        for &c in dir.dirs.iter().rev() {
            if !model.dirs[c as usize].purged && expanded[c as usize] {
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
    let mut sizes: Vec<u64> =
        model.dirs.iter().map(|d| d.files.iter().map(|&f| &model.files[f as usize]).filter(|f| !f.purged).map(|f| f.size).sum()).collect();
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
        LayoutParams { min_arc: 0.02, max_arc: 0.2, gap: 0.006, dir_marks: true, dir_mark_arc: 0.03, max_rings, max_tree_rings: 0 }
    }

    fn sample() -> Model {
        let mut m = Model::new("~", 0);
        let a = m.add_dir(0, "a");
        let b = m.add_dir(a, "b");
        let c = m.add_dir(a, "c");
        for i in 0..40 {
            m.add_existing_file(a, format!("f{i}.rs"), 1000 * i, 0);
            m.add_existing_file(b, format!("g{i}.png"), 2_000_000, 100);
            m.add_existing_file(c, format!("h{i}.png"), 2_000_000, 50);
        }
        m
    }

    #[test]
    fn segments_fit_rings_and_do_not_overlap() {
        let mut m = sample();
        assign_arcs(&mut m, &params(10_000), 0);
        let l = build(&m, &params(10_000), &[]);
        assert!(l.segments.iter().all(|s| s.a0 >= 0.0 && s.a1 <= 1.0 && s.a0 < s.a1));
        for w in l.segments.windows(2) {
            if w[0].ring == w[1].ring {
                assert!(w[0].a1 <= w[1].a0);
            }
        }
        assert!(l.seg_of_file.iter().all(|&s| s != NONE));
        assert_eq!(l.collapsed, 0);
    }

    #[test]
    fn collapses_to_fit_budget_preferring_recent_and_hot() {
        let mut m = sample();
        assign_arcs(&mut m, &params(10_000), 0);
        let full = build(&m, &params(10_000), &[]).rings.len();
        let l = build(&m, &params(full - 4), &[]);
        assert!(l.rings.len() <= full - 4 + 2);
        assert!(l.segments.iter().any(|s| s.flags & SEG_AGG != 0));
        assert!(l.seg_of_file.iter().all(|&s| s != NONE));
        // b (newest) stays expanded, c (older) collapses.
        assert!(l.is_expanded(2) && !l.is_expanded(3));
        assert!(l.agg_for(&m, 3).is_some());
        // Heat on c flips the choice.
        let mut hot = vec![0.0; m.dirs.len()];
        hot[3] = 5.0;
        let l = build(&m, &params(full - 4), &hot);
        assert!(l.is_expanded(3) && !l.is_expanded(2));
    }

    #[test]
    fn no_subtree_exceeds_the_cap() {
        // Two top-level trees of 30 folders x 30 files; plenty of budget.
        let mut m = Model::new("~", 0);
        for top in ["build", "docs"] {
            let t = m.add_dir(0, top);
            for i in 0..30 {
                let d = m.add_dir(t, format!("d{i}"));
                for j in 0..30 {
                    m.add_existing_file(d, format!("f{j}"), 5000, if top == "build" { 100 + i } else { i });
                }
            }
        }
        let mut p = params(100_000);
        assign_arcs(&mut m, &p, 0);
        let rings_under = |l: &Layout, top: u32| {
            l.rings.iter().filter(|r| std::iter::successors(Some(r.dir), |&d| m.dirs[d as usize].parent).any(|d| d == top)).count()
        };
        let full = build(&m, &p, &[]);
        assert!(rings_under(&full, 1) > 60 && full.collapsed == 0);
        p.max_tree_rings = 40;
        let l = build(&m, &p, &[]);
        for top in [1, 32] {
            let n = rings_under(&l, top);
            assert!(n <= 40 && n > 10, "tree {top}: {n} rings");
        }
        // The most recently changed folders are the ones kept.
        assert!(l.is_expanded(31) && !l.is_expanded(2));
        assert!(l.seg_of_file.iter().all(|&s| s != NONE));
    }
}
