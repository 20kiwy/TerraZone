//! Framework-free cameras + picking math — ported from the ArchCraft (effect) project's
//! `arch-arch/camera.rs` (which itself ported the whylia viewer): no egui types, so the same
//! model drives the 3D viewport, the Camera panel and the control protocol. The EffectCraft
//! viewer's own two camera modes ride along: the orbit arm and the first-person walk rig.

use glam::{Mat4, Vec3};

/// The 3D environment's camera mode — the EffectCraft viewer's pair: [`CamMode::Orbit`] swings
/// the orbit arm around its target, [`CamMode::Walk`] stands the camera on the ground and
/// drives it (WASD + drag look, Q/E down/up).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CamMode {
    /// The orbit arm ([`OrbitCamera`]) — the environment's original camera.
    #[default]
    Orbit,
    /// The first-person walk rig ([`WalkCam`]) — the EffectCraft camera.
    Walk,
}

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

    /// Stand a walk camera where the orbit eye is, looking the way the orbit looks — the
    /// EffectCraft viewer's orbit→walk handover, so switching modes doesn't jump the view.
    pub fn from_walk(w: &WalkCam) -> Self {
        let yaw = w.yaw + std::f32::consts::PI;
        let pitch = (-w.pitch).clamp(0.06, 1.5533);
        let f = w.forward_xz();
        let dist = (w.pos[1] * 2.5).clamp(25.0, 20_000.0);
        Self {
            target: [w.pos[0] + f[0] * dist, 0.0, w.pos[2] + f[1] * dist],
            dist,
            yaw,
            pitch,
            fov_y: w.fov_y,
            far_plane: 30_000.0,
            target_dist: None,
        }
    }
}

/// The first-person walk camera — the EffectCraft viewer's drive rig (ported from the
/// ArchCraft `WalkCam`): a position on/over the ground, a look yaw/pitch, one wide fov.
/// `yaw`/`pitch` follow the reference exactly (`yaw` 0 faces `+z`, `pitch` up is positive);
/// TerraZone's frame puts `+z` south, so the default faces north at `yaw = π`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WalkCam {
    /// The camera's position (world metres; `y` the eye height above the datum).
    pub pos: [f32; 3],
    /// Look yaw (radians; 0 faces `+z` — south in TerraZone's frame).
    pub yaw: f32,
    /// Look pitch (radians; positive looks up, clamped ±[`Self::PITCH_LIMIT`] by [`Self::look`]).
    pub pitch: f32,
    /// Vertical field of view (radians; the reference walks with a wide 65°).
    pub fov_y: f32,
}

impl Default for WalkCam {
    fn default() -> Self {
        Self { pos: [0.0, 1.6, 0.0], yaw: std::f32::consts::PI, pitch: 0.0, fov_y: 65.0f32.to_radians() }
    }
}

impl WalkCam {
    /// The look pitch clamp (±83°, the reference's ±1.45 rad).
    pub const PITCH_LIMIT: f32 = 1.45;
    /// The lowest eye height (metres) — the camera never sinks under the datum.
    pub const MIN_HEIGHT: f32 = 0.4;

    /// The unit look direction (`yaw`/`pitch` applied, the reference's own math).
    pub fn dir(&self) -> Vec3 {
        Vec3::new(self.yaw.sin() * self.pitch.cos(), self.pitch.sin(), self.yaw.cos() * self.pitch.cos()).normalize_or_zero()
    }

    /// The look direction flattened onto the ground (`[x, z]`, unit length).
    pub fn forward_xz(&self) -> [f32; 2] {
        [self.yaw.sin(), self.yaw.cos()]
    }

    /// The screen-right direction flattened onto the ground (`[x, z]`, unit length) — facing
    /// south (`yaw` 0) the right hand points west, exactly as the reference walks it.
    pub fn right_xz(&self) -> [f32; 2] {
        [-self.yaw.cos(), self.yaw.sin()]
    }

    /// Drag look: `dx`/`dy` in radians (the reference's `yaw += dx`, `pitch += dy` — dragging
    /// down looks up, the grab-the-world feel), pitch clamped.
    pub fn look(&mut self, dx: f32, dy: f32) {
        self.yaw += dx;
        self.pitch = (self.pitch + dy).clamp(-Self::PITCH_LIMIT, Self::PITCH_LIMIT);
    }

    /// Walk the ground: `forward`/`right` are the key axes (−1..1 each) scaled by `metres`.
    /// The height stays — walking doesn't fly.
    pub fn walk(&mut self, forward: f32, right: f32, metres: f32) {
        let f = self.forward_xz();
        let r = self.right_xz();
        self.pos[0] += (f[0] * forward + r[0] * right) * metres;
        self.pos[2] += (f[1] * forward + r[1] * right) * metres;
    }

    /// Lift or lower the camera (`metres` up positive), never under [`Self::MIN_HEIGHT`].
    pub fn lift(&mut self, metres: f32) {
        self.pos[1] = (self.pos[1] + metres).max(Self::MIN_HEIGHT);
    }

    /// The view matrix (right-handed look-at along [`Self::dir`], `y` up).
    pub fn view(&self) -> Mat4 {
        Mat4::look_at_rh(Vec3::from(self.pos), Vec3::from(self.pos) + self.dir(), Vec3::Y)
    }

    /// The perspective projection: the near plane hugs the eye, the far plane reaches past
    /// TerraZone's 60 km ground slab so the horizon stays drawn while walking.
    pub fn proj(&self, aspect: f32) -> Mat4 {
        Mat4::perspective_rh(self.fov_y, aspect, 0.1, 120_000.0)
    }

    /// Stand a walk camera at the orbit eye looking the orbit's way — the orbit→walk handover.
    pub fn from_orbit(c: &OrbitCamera) -> Self {
        let eye = c.eye();
        Self {
            pos: [eye.x, eye.y.max(Self::MIN_HEIGHT), eye.z],
            yaw: c.yaw + std::f32::consts::PI,
            pitch: (-c.pitch).clamp(-Self::PITCH_LIMIT, Self::PITCH_LIMIT),
            fov_y: c.fov_y.max(50.0f32.to_radians()),
        }
    }
}

/// The camera the 3D frame renders through: one of the two EffectCraft modes behind one
/// interface (the viewport, the pick rays and the ground grid read the rig, never the mode).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CameraRig {
    /// The orbit arm.
    Orbit(OrbitCamera),
    /// The first-person walk rig.
    Walk(WalkCam),
}

impl CameraRig {
    /// The rig for a mode and its two cameras.
    pub fn of(mode: CamMode, orbit: OrbitCamera, walk: WalkCam) -> Self {
        match mode {
            CamMode::Orbit => CameraRig::Orbit(orbit),
            CamMode::Walk => CameraRig::Walk(walk),
        }
    }

    /// The view matrix.
    pub fn view(&self) -> Mat4 {
        match self {
            CameraRig::Orbit(c) => c.view(),
            CameraRig::Walk(c) => c.view(),
        }
    }

    /// The perspective projection for one aspect ratio.
    pub fn proj(&self, aspect: f32) -> Mat4 {
        match self {
            CameraRig::Orbit(c) => c.proj(aspect),
            CameraRig::Walk(c) => c.proj(aspect),
        }
    }

    /// The eye position (the walk rig's position itself).
    pub fn eye(&self) -> Vec3 {
        match self {
            CameraRig::Orbit(c) => c.eye(),
            CameraRig::Walk(c) => Vec3::from(c.pos),
        }
    }

    /// The look direction flattened onto the ground (`[x, z]`) — the ground grid's horizon
    /// marching reads it.
    pub fn forward_xz(&self) -> [f32; 2] {
        match self {
            CameraRig::Orbit(c) => {
                let e = c.eye();
                let f = [c.target[0] - e.x, c.target[2] - e.z];
                let l = (f[0] * f[0] + f[1] * f[1]).sqrt().max(1e-6);
                [f[0] / l, f[1] / l]
            }
            CameraRig::Walk(c) => c.forward_xz(),
        }
    }

    /// A pick ray through an NDC point (`ndc` −1..1, y up) at one aspect, built from the
    /// camera's own basis (the proven TerraZone route — inverting the view-projection at
    /// sampled depths is convention-dependent).
    pub fn ray_from_ndc(&self, ndc: [f32; 2], aspect: f32) -> Ray {
        let (forward, origin, fov_y) = match self {
            CameraRig::Orbit(c) => ((Vec3::from(c.target) - c.eye()).normalize_or_zero(), c.eye(), c.fov_y),
            CameraRig::Walk(c) => (c.dir(), Vec3::from(c.pos), c.fov_y),
        };
        let right = forward.cross(Vec3::Y).normalize_or_zero();
        let up = right.cross(forward);
        let ty = (fov_y * 0.5).tan();
        let tx = ty * aspect;
        let dir = (forward + right * (ndc[0] * tx) + up * (ndc[1] * ty)).normalize_or_zero();
        Ray { origin, dir }
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

    #[test]
    fn the_walk_cam_faces_where_its_math_says() {
        // yaw 0 faces +z (south in TerraZone's frame); +90° turns toward +x (east); pitch up
        // is positive.
        let south = WalkCam { yaw: 0.0, ..Default::default() };
        let d = south.dir();
        assert!(d.z > 0.99 && d.x.abs() < 1e-4 && d.y.abs() < 1e-4, "yaw 0 looks south: {d:?}");
        let f = south.forward_xz();
        let r = south.right_xz();
        assert!(f[1] > 0.99, "forward is +z: {f:?}");
        assert!(r[0] < -0.99, "facing south the right hand is west (−x): {r:?}");
        let east = WalkCam { yaw: std::f32::consts::FRAC_PI_2, ..south };
        assert!(east.dir().x > 0.99, "yaw +90° looks east: {:?}", east.dir());
        let up = WalkCam { pitch: 1.0, ..south };
        assert!(up.dir().y > 0.8, "positive pitch looks up: {:?}", up.dir());
    }

    #[test]
    fn walk_keys_move_along_the_look_not_the_axes() {
        // Facing east (yaw 90°), W must walk east and D must walk south — the view's own
        // forward/right, never the world's.
        let mut cam = WalkCam { pos: [0.0, 1.6, 0.0], yaw: std::f32::consts::FRAC_PI_2, pitch: 0.0, fov_y: 1.0 };
        cam.walk(1.0, 0.0, 10.0);
        assert!(cam.pos[0] > 9.9 && cam.pos[2].abs() < 1e-3, "W walks the look direction: {:?}", cam.pos);
        assert_eq!(cam.pos[1], 1.6, "walking keeps the height");
        cam.walk(0.0, 1.0, 10.0);
        assert!(cam.pos[2] > 9.9, "D strafes screen-right (south when facing east): {:?}", cam.pos);
        // The height never sinks under the floor.
        cam.lift(-100.0);
        assert_eq!(cam.pos[1], WalkCam::MIN_HEIGHT, "lift clamps at the floor");
    }

    #[test]
    fn switching_modes_hands_the_view_over_without_a_jump() {
        // Orbit → walk: the walk camera stands at the orbit eye looking the orbit's way.
        let orbit = OrbitCamera { target: [500.0, 0.0, -300.0], dist: 900.0, yaw: 0.6, pitch: 0.62, ..Default::default() };
        let mut walk = WalkCam::from_orbit(&orbit);
        let look_o = (Vec3::from(orbit.target) - orbit.eye()).normalize_or_zero();
        let look_w = walk.dir();
        assert!(look_o.dot(look_w) > 0.99, "the walk looks where the orbit looked: {look_o:?} vs {look_w:?}");
        let e = orbit.eye();
        assert!((walk.pos[0] - e.x).abs() < 0.01 && (walk.pos[2] - e.z).abs() < 0.01, "the walk stands at the eye");
        // Walk → orbit: the orbit looks where the walk looked, pivoting just ahead of it.
        walk.walk(1.0, 0.0, 40.0);
        walk.look(0.3, 0.1);
        let back = OrbitCamera::from_walk(&walk);
        let look_w2 = walk.dir();
        let look_b = (Vec3::from(back.target) - back.eye()).normalize_or_zero();
        assert!(look_w2.dot(look_b) > 0.95, "the orbit looks where the walk looked: {look_w2:?} vs {look_b:?}");
        assert!(back.pitch >= 0.06, "the orbit pitch stays in its clamp: {}", back.pitch);
    }

    #[test]
    fn the_walk_rigs_rays_pick_the_ground_it_looks_at() {
        // A walk camera looking slightly down: the centre ray lands ahead of the camera, and
        // two different screen points land apart (the tools' pick the 3D canvas rides on).
        let rig = CameraRig::Walk(WalkCam { pos: [0.0, 40.0, 0.0], yaw: std::f32::consts::PI, pitch: -0.35, fov_y: 65.0f32.to_radians() });
        let ray = rig.ray_from_ndc([0.0, 0.0], 1.6);
        let hit = ray.plane_y(0.0).expect("a pitched-down walk ray must meet the ground");
        assert!(hit.z < -5.0, "facing north (−z) the hit is ahead: {hit:?}");
        let left = rig.ray_from_ndc([-0.5, 0.0], 1.6).plane_y(0.0).unwrap();
        assert!((left - hit).length() > 5.0, "different screen points pick apart: {left:?} vs {hit:?}");
        // The orbit rig's rays still work through the same door.
        let orbit = CameraRig::Orbit(OrbitCamera::default());
        let hit_o = orbit.ray_from_ndc([0.0, 0.0], 1.6).plane_y(0.0).expect("orbit ray hits");
        assert!(hit_o.length().is_finite());
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
