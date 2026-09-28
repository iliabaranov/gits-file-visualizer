//! Fixed per-monitor compositions. The camera is deliberately still: all motion
//! comes from the data (see docs/DESIGN.md §5.4).

use gits_core::config::Tower;
use glam::{Mat4, Vec3};

/// A camera framing taken from the film's shots.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Composition {
    /// The establishing shot: the whole tower in its chamber.
    Wide,
    /// Close to the tower side: the dense gold striations fill the frame.
    Drum,
    /// Straight down the axis: the rings become concentric circles.
    Dial,
}

impl Composition {
    /// Parses a config value; `None` for "auto" (resolved per output).
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "wide" | "band" => Some(Self::Wide),
            "drum" => Some(Self::Drum),
            "dial" => Some(Self::Dial),
            "auto" => None,
            other => {
                log::warn!("unknown composition '{other}', using 'wide'");
                Some(Self::Wide)
            }
        }
    }

    /// "auto" for a set of outputs `(width, height)`: portrait screens get the
    /// drum close-up, the largest landscape screen the wide shot, and other
    /// landscape screens the dial.
    pub fn auto(outputs: &[(u32, u32)]) -> Vec<Self> {
        let largest =
            outputs.iter().enumerate().filter(|(_, (w, h))| w >= h).max_by_key(|(_, (w, h))| *w as u64 * *h as u64).map(|(i, _)| i);
        outputs
            .iter()
            .enumerate()
            .map(|(i, (w, h))| {
                if h > w {
                    Self::Drum
                } else if Some(i) == largest || outputs.len() == 1 {
                    Self::Wide
                } else {
                    Self::Dial
                }
            })
            .collect()
    }
}

const WIDE_FOV: f32 = 43.0;
/// From the target toward the wide camera: 23° above the horizontal.
const WIDE_DIR: Vec3 = Vec3::new(0.0, 0.3907, 0.9205);
/// Largest |NDC| the tower may reach in the wide shot (the rest is margin).
const WIDE_FILL: [f32; 2] = [0.9, 0.92];

/// Distance and target height at which the whole tower (and the floor lights
/// around its foot) fills the wide shot, centred. Cached: it depends only on
/// the screen shape and the tower's size.
fn wide_fit(aspect: f32, tower: &Tower, y_top: f32) -> (f32, f32) {
    type Key = [u32; 4];
    thread_local! {
        static CACHE: std::cell::RefCell<Vec<(Key, (f32, f32))>> = const { std::cell::RefCell::new(Vec::new()) };
    }
    let key = [aspect.to_bits(), tower.radius.to_bits(), tower.height.to_bits(), y_top.to_bits()];
    if let Some(v) = CACHE.with(|c| c.borrow().iter().find(|(k, _)| *k == key).map(|e| e.1)) {
        return v;
    }
    let (top, bottom) = (y_top + 0.1, y_top - tower.height - 0.1);
    let r = tower.radius + 0.1;
    let mut points = Vec::with_capacity(144);
    for i in 0..48 {
        let a = std::f32::consts::TAU * i as f32 / 48.0;
        let (c, s) = (a.cos(), a.sin());
        points.push(Vec3::new(r * c, top, r * s));
        points.push(Vec3::new(r * c, bottom, r * s));
        // The floor lights (fx::build_background).
        let lr = (tower.radius + 0.6).max(2.3);
        points.push(Vec3::new(lr * c, bottom, lr * s));
    }
    let cam = |dist: f32, target_y: f32| Camera {
        eye: Vec3::Y * target_y + WIDE_DIR * dist,
        target: Vec3::new(0.0, target_y, 0.0),
        up: Vec3::Y,
        fov_y: WIDE_FOV.to_radians(),
        aspect,
        inside: false,
    };
    // Vertical NDC extent, or None if a point leaves the frame sideways or is behind.
    let extent = |c: &Camera| -> Option<(f32, f32)> {
        let vp = c.view_proj();
        let (mut lo, mut hi) = (f32::MAX, f32::MIN);
        for p in &points {
            let q = vp * p.extend(1.0);
            if q.w <= 1e-3 || (q.x / q.w).abs() > WIDE_FILL[0] {
                return None;
            }
            lo = lo.min(q.y / q.w);
            hi = hi.max(q.y / q.w);
        }
        Some((lo, hi))
    };
    let mut target_y = (top + bottom) * 0.5;
    let mut dist = 20.0;
    for _ in 0..4 {
        // Nearest distance at which everything fits...
        let (mut near, mut far) = (r + 1.0, 400.0);
        for _ in 0..28 {
            let mid = 0.5 * (near + far);
            match extent(&cam(mid, target_y)) {
                Some((lo, hi)) if lo >= -WIDE_FILL[1] && hi <= WIDE_FILL[1] => far = mid,
                _ => near = mid,
            }
        }
        dist = far;
        // ...then centre it vertically.
        let Some((lo, hi)) = extent(&cam(dist, target_y)) else { break };
        target_y += 0.5 * (lo + hi) * dist * (WIDE_FOV.to_radians() * 0.5).tan();
    }
    CACHE.with(|c| {
        let mut c = c.borrow_mut();
        if c.len() >= 8 {
            c.remove(0);
        }
        c.push((key, (dist, target_y)));
    });
    (dist, target_y)
}

#[derive(Clone, Copy, Debug)]
pub struct Camera {
    pub eye: Vec3,
    pub target: Vec3,
    pub up: Vec3,
    pub fov_y: f32,
    pub aspect: f32,
    /// Looking at the inside of the tower (dial): back faces get full weight.
    pub inside: bool,
}

impl Camera {
    pub fn new(c: Composition, aspect: f32, tower: &Tower, y_top: f32, drift: f32, t: f32) -> Self {
        match c {
            Composition::Wide => Self::wide(aspect, tower, y_top, drift, t),
            Composition::Drum => Self::drum(aspect, tower, y_top, drift, t),
            Composition::Dial => Self::dial(aspect, tower, y_top, drift, t),
        }
    }

    /// Optional drift: a few degrees of orbit over many minutes.
    fn drift_angle(drift: f32, t: f32) -> f32 {
        drift.clamp(0.0, 1.0) * 0.07 * (t * 0.0021).sin()
    }

    /// The "wide tower" composition from the film's establishing shot: the
    /// whole tower, top to floor lights, looking down on it (~23°) so the rings
    /// read as ellipses. The distance is fitted to the screen's shape.
    pub fn wide(aspect: f32, tower: &Tower, y_top: f32, drift: f32, t: f32) -> Self {
        let (dist, target_y) = wide_fit(aspect, tower, y_top);
        let a = Self::drift_angle(drift, t);
        let eye = Vec3::new(a.sin(), 0.0, a.cos()) * (dist * WIDE_DIR.z) + Vec3::Y * (target_y + dist * WIDE_DIR.y);
        Self { eye, target: Vec3::new(0.0, target_y, 0.0), up: Vec3::Y, fov_y: WIDE_FOV.to_radians(), aspect, inside: false }
    }

    /// Close to the side of the tower, looking slightly up along the packed
    /// drum, so the fine striations fill a tall frame.
    pub fn drum(aspect: f32, tower: &Tower, y_top: f32, drift: f32, t: f32) -> Self {
        // Beside the tower at the upper third, looking a little down past its
        // edge: the curved striations fill most of the frame, with dark space
        // and the calligraphy wall beside them.
        let target_y = y_top - tower.height * 0.35;
        let dist = tower.radius * 2.3 + 2.0 + if aspect > 1.0 { 2.5 } else { 0.0 };
        let a = Self::drift_angle(drift, t) + 0.5;
        let eye = Vec3::new(dist * a.sin(), target_y + 1.8, dist * a.cos());
        let side = Vec3::new(a.cos(), 0.0, -a.sin()) * (tower.radius * 0.55);
        Self { eye, target: Vec3::new(0.0, target_y - 1.0, 0.0) + side, up: Vec3::Y, fov_y: 50f32.to_radians(), aspect, inside: false }
    }

    /// Above the tower, looking down its axis: concentric rings narrowing into
    /// the depth, with the floor lights glowing at the centre.
    pub fn dial(aspect: f32, tower: &Tower, y_top: f32, drift: f32, t: f32) -> Self {
        let a = Self::drift_angle(drift, t);
        let eye = Vec3::new(0.0, y_top + tower.radius * 0.9, 0.0);
        let target = Vec3::new(0.0, y_top - tower.height, 0.0);
        let up = Vec3::new(a.sin(), 0.0, -a.cos());
        Self { eye, target, up, fov_y: 78f32.to_radians(), aspect, inside: true }
    }

    pub fn view(&self) -> Mat4 {
        glam::camera::rh::view::look_at_mat4(self.eye, self.target, self.up)
    }

    pub fn proj(&self) -> Mat4 {
        // wgpu clip space: depth 0..1, like Direct3D.
        glam::camera::rh::proj::directx::perspective(self.fov_y, self.aspect.max(0.05), 0.1, 200.0)
    }

    pub fn view_proj(&self) -> Mat4 {
        self.proj() * self.view()
    }

    pub fn right_up(&self) -> (Vec3, Vec3) {
        let fwd = (self.target - self.eye).normalize();
        let right = fwd.cross(self.up).normalize();
        let up = right.cross(fwd).normalize();
        (right, up)
    }

    /// World point → pixel coordinates (origin top-left) and whether it is in front.
    pub fn project(&self, p: Vec3, width: f32, height: f32) -> Option<(f32, f32)> {
        let c = self.view_proj() * p.extend(1.0);
        if c.w <= 1e-4 {
            return None;
        }
        let ndc = c.truncate() / c.w;
        Some(((ndc.x * 0.5 + 0.5) * width, (0.5 - ndc.y * 0.5) * height))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_assignment() {
        // This machine: 1920x1200, 3840x2160, 1440x2560 (portrait).
        let c = Composition::auto(&[(1920, 1200), (3840, 2160), (1440, 2560)]);
        assert_eq!(c, vec![Composition::Dial, Composition::Wide, Composition::Drum]);
        assert_eq!(Composition::auto(&[(1920, 1080)]), vec![Composition::Wide]);
    }

    #[test]
    fn wide_shows_the_whole_tower() {
        let cfg = gits_core::config::Config::default();
        let tower = &cfg.tower;
        let y_top = tower.height - 3.2;
        for (w, h) in [(3840.0, 2160.0), (1920.0, 1200.0), (1080.0, 1920.0), (5120.0, 1440.0)] {
            let cam = Camera::wide(w / h, tower, y_top, 0.0, 0.0);
            let (mut lo, mut hi) = (f32::MAX, f32::MIN);
            for i in 0..64 {
                let a = std::f32::consts::TAU * i as f32 / 64.0;
                for y in [y_top, y_top - tower.height] {
                    let (sx, sy) = cam.project(Vec3::new(tower.radius * a.cos(), y, tower.radius * a.sin()), w, h).unwrap();
                    assert!((0.0..w).contains(&sx) && (0.0..h).contains(&sy), "{w}x{h}: ({sx}, {sy}) off screen");
                    lo = lo.min(sy);
                    hi = hi.max(sy);
                }
            }
            // Fills most of the height (or width), roughly centred.
            assert!(hi - lo > 0.6 * h || w < h, "{w}x{h}: tower only {:.0}..{:.0}", lo, hi);
            assert!((lo - (h - hi)).abs() < 0.15 * h, "{w}x{h}: not centred ({lo:.0}, {hi:.0})");
        }
    }
}
