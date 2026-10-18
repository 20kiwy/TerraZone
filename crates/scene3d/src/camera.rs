//! Framework-free orbit camera + picking math — ported from the ArchCraft (effect) project's
//! `arch-arch/camera.rs` (which itself ported the whylia viewer): no egui types, so the same
//! model drives the 3D viewport, the Camera panel and the control protocol.

use glam::{Mat4, Vec3};

/// An orbit camera: `target` on the ground, `dist` back along a yaw/pitch arm, `fov_y` wide.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OrbitCamera {
    /// The point the camera looks at (world metres; `y` the ground height it orbits).
    pub target: [f32; 3],
    /// Eye distance from the target (metres).
    pub dist: f32,
    /// Orbit yaw (radians, 0 = looking north at the target from the south).
    pub yaw: f32,
    /// Orbit pitch above the ground plane (radians; 0 = ground level, π/2 = top-down).
    pub pitch: f32,
    /// Vertical field of view (radians).
    pub fov_y: f32,
    /// Far clip (metres; widened to fit `dist`).
    pub far_plane: f32,
    /// Where an eased dolly is heading (`None` = dist is current). The wheel sets this and the
    /// frame walks `dist` toward it — the reference engine's eased camera, so a zoom glide
    /// instead of snapping.
    pub target_dist: Option<f32>,
}

impl Default for OrbitCamera {
    fn default() -> Self {
        Self { target: [0.0, 0.0, 0.0], dist: 900.0, yaw: 0.6, pitch: 0.62, fov_y: 50.0f32.to_radians(), far_plane: 30_000.0, target_dist: None }
    }
}

impl OrbitCamera {
    /// The eye position the orbit arm puts the camera at.
    pub fn eye(&self) -> Vec3 {
        let cp = self.pitch.cos();
        Vec3::new(
            self.target[0] + self.dist * cp * self.yaw.sin(),
            self.target[1] + self.dist * self.pitch.sin(),
            self.target[2] + self.dist * cp * self.yaw.cos(),
        )
    }

    /// The view matrix (right-handed look-at, `y` up).
    pub fn view(&self) -> Mat4 {
        Mat4::look_at_rh(self.eye(), Vec3::from(self.target), Vec3::Y)
    }

    /// The perspective projection for one aspect ratio.
    pub fn proj(&self, aspect: f32) -> Mat4 {
        let far = self.far_plane.max(self.dist * 8.0 + 400.0);
        let near = (self.dist * 0.005).max(0.05);
        Mat4::perspective_rh(self.fov_y, aspect, near, far)
    }

    /// Frame a world-space bounding box: the target becomes its centre, the distance its radius.
    pub fn frame_bounds(&mut self, min: [f32; 3], max: [f32; 3]) {
        let center = [(min[0] + max[0]) * 0.5, (min[1] + max[1]) * 0.5, (min[2] + max[2]) * 0.5];
        let extent = [(max[0] - min[0]).abs(), (max[1] - min[1]).abs(), (max[2] - min[2]).abs()];
        let radius = extent.iter().fold(0.0f32, |r, e| r.max(*e)) * 0.5;
        self.target = center;
        self.dist = (radius / self.fov_y.sin().max(0.05)) * 1.2;
        self.target_dist = None;
    }

    /// Orbit: `dyaw`/`dpitch` radians about the target, pitch clamped just over the ground.
    pub fn orbit(&mut self, dyaw: f32, dpitch: f32) {
        self.yaw += dyaw;
        self.pitch = (self.pitch + dpitch).clamp(0.06, 1.5533);
    }

    /// Move the target along the ground in view space (`dx` right, `dz` toward the viewer),
    /// scaled by the distance so it feels the same at any zoom.
    pub fn pan(&mut self, dx: f32, dz: f32) {
        let scale = self.dist * 0.0016;
        let (sy, cy) = self.yaw.sin_cos();
        // Screen right = (cy, 0, -sy)·? — yaw 0 looks north (−z): right is +x.
        let right = Vec3::new(cy, 0.0, -sy);
        let forward = Vec3::new(sy, 0.0, cy); // toward the viewer (south when yaw = 0)
        let t = Vec3::from(self.target) - right * (dx * scale) + forward * (dz * scale);
        self.target = [t.x, self.target[1], t.z];
    }

    /// Dolly by a multiplicative factor, clamped to a sane range.
    pub fn dolly_by(&mut self, factor: f32) {
        let base = self.target_dist.unwrap_or(self.dist);
        self.target_dist = Some((base * factor).clamp(5.0, 200_000.0));
    }

    /// The frame's eased step toward an in-flight dolly target (`true` while still gliding).
    /// A direct `dist` write (framing, a command) clears the target instead — the explicit
    /// move wins.
    pub fn ease_dolly(&mut self) -> bool {
        let Some(target) = self.target_dist else { return false };
        let d = target - self.dist;
        if d.abs() <= target.abs() * 0.01 + 0.5 {
            self.dist = target;
            self.target_dist = None;
            return false;
        }
        self.dist = (self.dist + d * 0.35).clamp(5.0, 200_000.0);
        true
    }
}

/// A pick ray from the eye through a point on the near plane.
#[derive(Debug, Clone, Copy)]
pub struct Ray {
    /// The ray origin (the eye).
    pub origin: Vec3,
    /// The normalized direction.
    pub dir: Vec3,
}

impl Ray {
    /// A point `t` metres along the ray.
    pub fn at(&self, t: f32) -> Vec3 {
        self.origin + self.dir * t
    }

    /// A ray from an orbit camera through an NDC point (`ndc` −1..1, y up) at one aspect. Built
    /// from the camera's own basis (forward/right/up and the fov's half-tangents) — inverting
    /// the view-projection at sampled NDC depths is convention-dependent and put every pick on
    /// the target.
    pub fn from_ndc(cam: &OrbitCamera, ndc: [f32; 2], aspect: f32) -> Ray {
        let forward = (Vec3::from(cam.target) - cam.eye()).normalize_or_zero();
        let right = forward.cross(Vec3::Y).normalize_or_zero();
        let up = right.cross(forward);
        let ty = (cam.fov_y * 0.5).tan();
        let tx = ty * aspect;
        let dir = (forward + right * (ndc[0] * tx) + up * (ndc[1] * ty)).normalize_or_zero();
        Ray { origin: cam.eye(), dir }
    }

    /// Where the ray meets the flat ground plane `y`, if it slopes into it.
    pub fn plane_y(&self, y: f32) -> Option<Vec3> {
        ray_plane_y(self, y)
    }
}

/// Where a ray crosses the flat plane `y` (world metres), if it does going forward.
pub fn ray_plane_y(ray: &Ray, y: f32) -> Option<Vec3> {
    let dy = ray.dir.y;
    if dy.abs() < 1.0e-6 {
        return None;
    }
    let t = (y - ray.origin.y) / dy;
    (t >= 0.0).then(|| ray.at(t))
}

/// Does a ray cross the axis-aligned box (metres)? (A fast slab test.)
pub fn ray_aabb(ray: &Ray, min: [f32; 3], max: [f32; 3]) -> bool {
    let mut tmin = 0.0f32;
    let mut tmax = f32::INFINITY;
    for a in 0..3 {
        let inv_d = 1.0 / ray.dir[a];
        let (mut t0, mut t1) = ((min[a] - ray.origin[a]) * inv_d, (max[a] - ray.origin[a]) * inv_d);
        if inv_d < 0.0 {
            std::mem::swap(&mut t0, &mut t1);
        }
        tmin = tmin.max(t0);
        tmax = tmax.min(t1);
        if tmax < tmin {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_eye_orbits_at_the_distance_it_says() {
        let cam = OrbitCamera::default();
        let eye = cam.eye();
        let d = (eye - Vec3::from(cam.target)).length();
        assert!((d - cam.dist).abs() < 0.01);
        // Yaw 0: the eye sits south of the target (z grows south), level by the pitch.
        let cam0 = OrbitCamera { yaw: 0.0, ..cam };
        let e0 = cam0.eye();
        assert!(e0.z > cam0.target[2] && e0.x.abs() < 1.0e-3);
    }

    #[test]
    fn a_ray_through_the_screen_centre_hits_the_ground_under_the_target() {
        let cam = OrbitCamera::default();
        let ray = Ray::from_ndc(&cam, [0.0, 0.0], 1.6);
        let hit = ray.plane_y(0.0).expect("a pitched camera ray must cross the ground");
        // It lands near the target, not kilometres away.
        let d = (hit - Vec3::from(cam.target)).length();
        assert!(d < cam.dist, "hit {hit:?} too far from {target:?}", target = cam.target);
    }

    #[test]
    fn ray_aabb_hits_and_misses() {
        let cam = OrbitCamera::default();
        let ray = Ray::from_ndc(&cam, [0.0, 0.0], 1.6);
        let hit = ray.plane_y(0.0).unwrap();
        let min = [hit.x - 2.0, -1.0, hit.z - 2.0];
        let max = [hit.x + 2.0, 6.0, hit.z + 2.0];
        assert!(ray_aabb(&ray, min, max));
        let far = [hit.x + 900.0, -1.0, hit.z + 900.0];
        let far_max = [hit.x + 902.0, 6.0, hit.z + 902.0];
        assert!(!ray_aabb(&ray, far, far_max));
    }

    #[test]
    fn pan_moves_the_target_across_the_ground_not_into_the_sky() {
        let mut cam = OrbitCamera::default();
        let before = cam.target;
        cam.pan(100.0, 0.0);
        assert_eq!(cam.target[1], before[1], "pan stays on the ground");
        assert!((cam.target[0] - before[0]).abs() > 0.0);
    }
}

#[cfg(test)]
mod probe_tests {
    use super::*;

    /// Two rays through different screen points must hit the ground at different points
    /// (the pick the 3D viewport's tools and readout ride on).
    #[test]
    fn rays_through_different_screen_points_hit_different_ground_points() {
        let cam = OrbitCamera {
            target: [0.0, 0.0, 0.0],
            dist: 140.0,
            yaw: 3.5777,
            pitch: std::f32::consts::FRAC_PI_6 + 0.0032,
            fov_y: 50.0f32.to_radians(),
            far_plane: 30_000.0,
            target_dist: None,
        };
        let (w, h) = (1058.0f32, 815.0f32);
        let ndc = |p: [f32; 2]| [((p[0] - 42.0) / w) * 2.0 - 1.0, -(((p[1] - 55.0) / h) * 2.0 - 1.0)];
        let a = Ray::from_ndc(&cam, ndc([450.0, 380.0]), w / h).plane_y(0.0).expect("hit");
        let b = Ray::from_ndc(&cam, ndc([850.0, 640.0]), w / h).plane_y(0.0).expect("hit");
        let d = (a - b).length();
        assert!(d > 5.0, "two screen points 460 pt apart must pick distinct ground points; got {a:?} vs {b:?} (Δ {d} m)");
    }
}
