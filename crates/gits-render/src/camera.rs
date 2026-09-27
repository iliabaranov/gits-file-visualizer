//! Fixed per-monitor compositions. The camera is deliberately still: all motion
//! comes from the data (see docs/DESIGN.md §5.4).

use glam::{Mat4, Vec3};

#[derive(Clone, Copy, Debug)]
pub struct Camera {
    pub eye: Vec3,
    pub target: Vec3,
    pub fov_y: f32,
    pub aspect: f32,
}

impl Camera {
    /// The "wide tower" composition from the film's establishing shot.
    pub fn wide(aspect: f32, y_top: f32, drift: f32, t: f32) -> Self {
        // Frame the bottom of the tower, the floor lights and the lattice, with the
        // top of the tower leaving the frame. Portrait screens pull back a little.
        // Looking down on the coil (~23°) so rings read as ellipses, as in the film.
        let pull = if aspect < 1.0 { 1.0 + (1.0 - aspect) * 0.9 } else { 1.0 };
        let dist = 11.6 * pull;
        let target_y = 0.2 + (y_top - 8.8) * 0.3 + if aspect < 1.0 { 1.0 } else { 0.0 };
        // Optional drift: a few degrees of orbit over many minutes.
        let a = drift.clamp(0.0, 1.0) * 0.07 * (t * 0.0021).sin();
        let eye = Vec3::new(dist * a.sin(), target_y + 5.0 * pull, dist * a.cos());
        Self { eye, target: Vec3::new(0.0, target_y, 0.0), fov_y: 43f32.to_radians(), aspect }
    }

    pub fn view(&self) -> Mat4 {
        glam::camera::rh::view::look_at_mat4(self.eye, self.target, Vec3::Y)
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
        let right = fwd.cross(Vec3::Y).normalize();
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
