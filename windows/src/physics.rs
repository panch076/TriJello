use std::sync::OnceLock;

pub const GRID_N: usize = 12;
pub const VERTS: usize = GRID_N * GRID_N;

#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn contains(&self, p: [f32; 2]) -> bool {
        p[0] >= self.x && p[0] < self.x + self.w && p[1] >= self.y && p[1] < self.y + self.h
    }

    pub fn intersects(&self, o: &Rect) -> bool {
        self.x < o.x + o.w && o.x < self.x + self.w && self.y < o.y + o.h && o.y < self.y + self.h
    }
}

type V2 = [f32; 2];

fn sub(a: V2, b: V2) -> V2 { [a[0] - b[0], a[1] - b[1]] }
fn add(a: V2, b: V2) -> V2 { [a[0] + b[0], a[1] + b[1]] }
fn scale(a: V2, s: f32) -> V2 { [a[0] * s, a[1] * s] }
fn len(a: V2) -> f32 { (a[0] * a[0] + a[1] * a[1]).sqrt() }

pub fn uvs() -> &'static [V2; VERTS] {
    static UVS: OnceLock<[V2; VERTS]> = OnceLock::new();
    UVS.get_or_init(|| {
        let d = (GRID_N - 1) as f32;
        std::array::from_fn(|i| [(i % GRID_N) as f32 / d, (i / GRID_N) as f32 / d])
    })
}

/// (a, b, relative stiffness): structural + shear springs
fn springs() -> &'static [(usize, usize, f32)] {
    static SPRINGS: OnceLock<Vec<(usize, usize, f32)>> = OnceLock::new();
    SPRINGS.get_or_init(|| {
        let mut s = Vec::new();
        for y in 0..GRID_N {
            for x in 0..GRID_N {
                let i = y * GRID_N + x;
                if x + 1 < GRID_N { s.push((i, i + 1, 1.0)); }
                if y + 1 < GRID_N { s.push((i, i + GRID_N, 1.0)); }
                if x + 1 < GRID_N && y + 1 < GRID_N {
                    s.push((i, i + GRID_N + 1, 0.5));
                    s.push((i + 1, i + GRID_N, 0.5));
                }
            }
        }
        s
    })
}

/// A mass-spring "jello" grid laid over a window rect. Positions are in global
/// physical pixels (top-left origin), same as DWM extended frame bounds.
pub struct JelloBody {
    pub pos: [V2; VERTS],
    vel: [V2; VERTS],
    rect: Rect,
    pub grab_index: Option<usize>,
    resting: bool,
}

impl JelloBody {
    pub fn new(rect: Rect, pop: bool) -> Self {
        let rest = Self::rest_positions(&rect);
        let pos = if pop {
            let c = [rect.x + rect.w / 2.0, rect.y + rect.h / 2.0];
            rest.map(|p| add(c, scale(sub(p, c), 0.7)))
        } else {
            rest
        };
        Self { pos, vel: [[0.0; 2]; VERTS], rect, grab_index: None, resting: false }
    }

    fn rest_positions(r: &Rect) -> [V2; VERTS] {
        uvs().map(|uv| [r.x + uv[0] * r.w, r.y + uv[1] * r.h])
    }

    pub fn grab(&mut self, p: V2) {
        let rest = Self::rest_positions(&self.rect);
        let d2 = |i: usize| {
            let d = sub(rest[i], p);
            d[0] * d[0] + d[1] * d[1]
        };
        self.grab_index = (0..VERTS).min_by(|&a, &b| d2(a).total_cmp(&d2(b)));
        self.resting = false;
    }

    /// Advances the simulation; returns false when the mesh didn't move (nothing to redraw).
    pub fn step(&mut self, dt: f32, new_rect: Rect, jello: bool) -> bool {
        if new_rect != self.rect {
            self.rect = new_rect;
            self.resting = false;
        }
        if self.grab_index.is_some() { self.resting = false; }
        if self.resting { return false; }
        let rest = Self::rest_positions(&self.rect);
        if !jello {
            self.pos = rest;
            self.vel.fill([0.0; 2]);
            self.resting = true;
            return true;
        }

        let substeps = 4;
        let h = dt / substeps as f32;
        let k_neighbor = 700.0;
        let k_home = if self.grab_index.is_none() { 90.0 } else { 6.0 };
        let damping = (-5.5 * h).exp();
        let mut force = [[0.0f32; 2]; VERTS];

        for _ in 0..substeps {
            for i in 0..VERTS { force[i] = scale(sub(rest[i], self.pos[i]), k_home); }
            for &(a, b, w) in springs() {
                let f = scale(sub(sub(self.pos[b], self.pos[a]), sub(rest[b], rest[a])), k_neighbor * w);
                force[a] = add(force[a], f);
                force[b] = sub(force[b], f);
            }
            for i in 0..VERTS {
                if Some(i) == self.grab_index {
                    self.pos[i] = rest[i];
                    self.vel[i] = [0.0; 2];
                    continue;
                }
                self.vel[i] = scale(add(self.vel[i], scale(force[i], h)), damping);
                self.pos[i] = add(self.pos[i], scale(self.vel[i], h));
            }
        }

        if self.grab_index.is_none() {
            let max_err = (0..VERTS)
                .map(|i| len(sub(self.pos[i], rest[i])) + len(self.vel[i]) * 0.02)
                .fold(0.0f32, f32::max);
            if max_err < 0.05 {
                self.pos = rest;
                self.vel.fill([0.0; 2]);
                self.resting = true;
            }
        }
        true
    }
}
