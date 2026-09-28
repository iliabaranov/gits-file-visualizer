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

    /// The "wide tower" composition from the film's establishing shot.
    pub fn wide(aspect: f32, tower: &Tower, y_top: f32, drift: f32, t: f32) -> Self {
        // Frame the bottom of the tower, the floor lights and the lattice, with the
        // top of the tower leaving the frame. Portrait screens pull back a little.
        // Looking down on the coil (~23°) so rings read as ellipses, as in the film.
        let pull = if aspect < 1.0 { 1.0 + (1.0 - aspect) * 0.9 } else { 1.0 };
        let dist = (11.6 + (tower.radius - 3.75).max(0.0) * 1.5) * pull;
        let target_y = 0.2 + (y_top - 8.8) * 0.3 + if aspect < 1.0 { 1.0 } else { 0.0 };
        let a = Self::drift_angle(drift, t);
        let eye = Vec3::new(dist * a.sin(), target_y + 5.0 * pull, dist * a.cos());
        Self { eye, target: Vec3::new(0.0, target_y, 0.0), up: Vec3::Y, fov_y: 43f32.to_radians(), aspect, inside: false }
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
}
