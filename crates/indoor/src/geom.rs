//! Small vector/segment/polygon helpers and a uniform bucket grid.

use serde::{Deserialize, Serialize};

pub type P2 = [f64; 2];
pub type P3 = [f64; 3];
pub const NONE: u32 = u32::MAX;

#[inline]
pub fn xy(p: P3) -> P2 {
    [p[0], p[1]]
}
#[inline]
pub fn sub(a: P2, b: P2) -> P2 {
    [a[0] - b[0], a[1] - b[1]]
}
#[inline]
pub fn add(a: P2, b: P2) -> P2 {
    [a[0] + b[0], a[1] + b[1]]
}
#[inline]
pub fn scale(a: P2, s: f64) -> P2 {
    [a[0] * s, a[1] * s]
}
#[inline]
pub fn dot(a: P2, b: P2) -> f64 {
    a[0] * b[0] + a[1] * b[1]
}
#[inline]
pub fn len(a: P2) -> f64 {
    // sqrt, not hypot: hypot is a slow libm call and inputs here never overflow.
    (a[0] * a[0] + a[1] * a[1]).sqrt()
}
#[inline]
pub fn dist(a: P2, b: P2) -> f64 {
    let (dx, dy) = (a[0] - b[0], a[1] - b[1]);
    (dx * dx + dy * dy).sqrt()
}
#[inline]
pub fn dist3(a: P3, b: P3) -> f64 {
    let (dx, dy, dz) = (a[0] - b[0], a[1] - b[1], a[2] - b[2]);
    (dx * dx + dy * dy + dz * dz).sqrt()
}
#[inline]
pub fn norm(a: P2) -> P2 {
    let l = len(a);
    if l > 1e-12 {
        [a[0] / l, a[1] / l]
    } else {
        [0.0, 0.0]
    }
}
#[inline]
pub fn lerp(a: P2, b: P2, t: f64) -> P2 {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]
}
#[inline]
pub fn lerp3(a: P3, b: P3, t: f64) -> P3 {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}
/// > 0 when `b` is left of the directed line o -> a.
#[inline]
pub fn cross(o: P2, a: P2, b: P2) -> f64 {
    (a[0] - o[0]) * (b[1] - o[1]) - (a[1] - o[1]) * (b[0] - o[0])
}

pub fn poly_len3(pts: &[P3]) -> f64 {
    pts.windows(2).map(|w| dist3(w[0], w[1])).sum()
}
pub fn poly_len2(pts: &[P3]) -> f64 {
    pts.windows(2).map(|w| dist(xy(w[0]), xy(w[1]))).sum()
}

/// Even-odd point in polygon over all rings (outer + holes).
pub fn in_rings(p: P2, rings: &[Vec<P2>]) -> bool {
    let mut inside = false;
    for ring in rings {
        let n = ring.len();
        let mut j = n - 1;
        for i in 0..n {
            let (a, b) = (ring[i], ring[j]);
            if (a[1] > p[1]) != (b[1] > p[1])
                && p[0] < (b[0] - a[0]) * (p[1] - a[1]) / (b[1] - a[1]) + a[0]
            {
                inside = !inside;
            }
            j = i;
        }
    }
    inside
}

pub fn ring_area(ring: &[P2]) -> f64 {
    let n = ring.len();
    let mut s = 0.0;
    for i in 0..n {
        let (a, b) = (ring[i], ring[(i + 1) % n]);
        s += a[0] * b[1] - b[0] * a[1];
    }
    0.5 * s
}

/// (closest point, parameter along a->b, squared distance).
pub fn closest_on_seg(p: P2, a: P2, b: P2) -> (P2, f64, f64) {
    let d = sub(b, a);
    let l2 = dot(d, d);
    let t = if l2 > 0.0 {
        (dot(sub(p, a), d) / l2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let q = lerp(a, b, t);
    let e = sub(p, q);
    (q, t, dot(e, e))
}

/// Inside a CCW triangle with a distance tolerance `tol` (metres).
#[inline]
pub fn in_tri(p: P2, a: P2, b: P2, c: P2, tol: f64) -> bool {
    cross(a, b, p) >= -tol * dist(a, b)
        && cross(b, c, p) >= -tol * dist(b, c)
        && cross(c, a, p) >= -tol * dist(c, a)
}

/// Closest point of a triangle (squared distance).
pub fn closest_on_tri(p: P2, a: P2, b: P2, c: P2) -> (P2, f64) {
    if in_tri(p, a, b, c, 0.0) {
        return (p, 0.0);
    }
    let mut best = closest_on_seg(p, a, b);
    for (u, v) in [(b, c), (c, a)] {
        let r = closest_on_seg(p, u, v);
        if r.2 < best.2 {
            best = r;
        }
    }
    (best.0, best.2)
}

/// z of `p` on the plane through the triangle.
pub fn bary_z(p: P2, a: P3, b: P3, c: P3) -> f64 {
    let det = (b[1] - c[1]) * (a[0] - c[0]) + (c[0] - b[0]) * (a[1] - c[1]);
    if det.abs() < 1e-18 {
        return (a[2] + b[2] + c[2]) / 3.0;
    }
    let l1 = ((b[1] - c[1]) * (p[0] - c[0]) + (c[0] - b[0]) * (p[1] - c[1])) / det;
    let l2 = ((c[1] - a[1]) * (p[0] - c[0]) + (a[0] - c[0]) * (p[1] - c[1])) / det;
    l1 * a[2] + l2 * b[2] + (1.0 - l1 - l2) * c[2]
}

/// Parameter along a->b where the line p->q crosses it (None if parallel).
pub fn line_cross_t(p: P2, q: P2, a: P2, b: P2) -> Option<f64> {
    let r = sub(q, p);
    let s = sub(b, a);
    let den = r[0] * s[1] - r[1] * s[0];
    if den.abs() < 1e-18 {
        return None;
    }
    let w = sub(a, p);
    Some((w[0] * r[1] - w[1] * r[0]) / den)
}

/// Heading in radians of a->b.
#[inline]
pub fn heading(a: P2, b: P2) -> f32 {
    (b[1] - a[1]).atan2(b[0] - a[0]) as f32
}

/// Uniform bucket grid over item bounding boxes (CSR buckets).
#[derive(Serialize, Deserialize, Default, Clone, Debug)]
pub struct Grid {
    pub x0: f64,
    pub y0: f64,
    pub cell: f64,
    pub nx: u32,
    pub ny: u32,
    pub off: Vec<u32>,
    pub items: Vec<u32>,
}

impl Grid {
    /// `boxes`: [xmin, ymin, xmax, ymax] per item.
    pub fn build(boxes: &[[f64; 4]]) -> Grid {
        if boxes.is_empty() {
            return Grid::default();
        }
        let mut bb = [
            f64::INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NEG_INFINITY,
        ];
        let mut size = 0.0;
        for b in boxes {
            bb[0] = bb[0].min(b[0]);
            bb[1] = bb[1].min(b[1]);
            bb[2] = bb[2].max(b[2]);
            bb[3] = bb[3].max(b[3]);
            size += (b[2] - b[0]).max(b[3] - b[1]);
        }
        let n = boxes.len() as f64;
        let (w, h) = ((bb[2] - bb[0]).max(1e-6), (bb[3] - bb[1]).max(1e-6));
        let mut cell = (w * h / n).sqrt().max(size / n).max(1e-3);
        loop {
            let nx = ((w / cell).floor() as u64 + 1).min(1 << 15);
            let ny = ((h / cell).floor() as u64 + 1).min(1 << 15);
            let mut g = Grid {
                x0: bb[0],
                y0: bb[1],
                cell,
                nx: nx as u32,
                ny: ny as u32,
                off: vec![],
                items: vec![],
            };
            let mut total = 0u64;
            for b in boxes {
                let (i0, i1, j0, j1) = g.range(b);
                total += ((i1 - i0 + 1) * (j1 - j0 + 1)) as u64;
            }
            if total > 24 * boxes.len() as u64 + 1024 || nx * ny > 8 * boxes.len() as u64 + 1024 {
                cell *= 1.6;
                continue;
            }
            let ncell = (nx * ny) as usize;
            let mut count = vec![0u32; ncell + 1];
            for b in boxes {
                let (i0, i1, j0, j1) = g.range(b);
                for j in j0..=j1 {
                    for i in i0..=i1 {
                        count[j * nx as usize + i + 1] += 1;
                    }
                }
            }
            for k in 1..=ncell {
                count[k] += count[k - 1];
            }
            let mut fill = count.clone();
            let mut items = vec![0u32; total as usize];
            for (k, b) in boxes.iter().enumerate() {
                let (i0, i1, j0, j1) = g.range(b);
                for j in j0..=j1 {
                    for i in i0..=i1 {
                        let c = j * nx as usize + i;
                        items[fill[c] as usize] = k as u32;
                        fill[c] += 1;
                    }
                }
            }
            g.off = count;
            g.items = items;
            return g;
        }
    }

    #[inline]
    fn ci(&self, v: f64, o: f64, n: u32) -> usize {
        (((v - o) / self.cell).floor().max(0.0) as usize).min(n as usize - 1)
    }

    fn range(&self, b: &[f64; 4]) -> (usize, usize, usize, usize) {
        (
            self.ci(b[0], self.x0, self.nx),
            self.ci(b[2], self.x0, self.nx),
            self.ci(b[1], self.y0, self.ny),
            self.ci(b[3], self.y0, self.ny),
        )
    }

    /// Items whose box covers the cell of `p` (empty outside the grid).
    #[inline]
    pub fn at(&self, p: P2) -> &[u32] {
        if self.nx == 0 {
            return &[];
        }
        let (fx, fy) = ((p[0] - self.x0) / self.cell, (p[1] - self.y0) / self.cell);
        if fx < 0.0 || fy < 0.0 || fx >= self.nx as f64 || fy >= self.ny as f64 {
            return &[];
        }
        let c = fy as usize * self.nx as usize + fx as usize;
        &self.items[self.off[c] as usize..self.off[c + 1] as usize]
    }

    /// Visit items of all cells overlapping the box (items may repeat).
    pub fn visit(&self, b: [f64; 4], mut f: impl FnMut(u32)) {
        if self.nx == 0 {
            return;
        }
        let gx1 = self.x0 + self.cell * self.nx as f64;
        let gy1 = self.y0 + self.cell * self.ny as f64;
        if b[2] < self.x0 || b[3] < self.y0 || b[0] > gx1 || b[1] > gy1 {
            return;
        }
        let (i0, i1, j0, j1) = self.range(&b);
        for j in j0..=j1 {
            for i in i0..=i1 {
                let c = j * self.nx as usize + i;
                for &it in &self.items[self.off[c] as usize..self.off[c + 1] as usize] {
                    f(it);
                }
            }
        }
    }
}

pub fn bbox(pts: impl IntoIterator<Item = P2>) -> [f64; 4] {
    let mut b = [
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    ];
    for p in pts {
        b[0] = b[0].min(p[0]);
        b[1] = b[1].min(p[1]);
        b[2] = b[2].max(p[0]);
        b[3] = b[3].max(p[1]);
    }
    b
}

/// Tiny deterministic PRNG (xorshift64*), used by tests.
pub struct Rng(pub u64);
impl Rng {
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }
    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n.max(1) as u64) as usize
    }
    pub fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// Min-heap of `u64` keys with four children per node: fewer levels than a binary heap, and
/// the four children of a node share a cache line, which makes pops cheaper in searches.
#[derive(Default, Clone, Debug)]
pub struct MinHeap {
    v: Vec<u64>,
}

impl MinHeap {
    pub fn new() -> MinHeap {
        MinHeap { v: Vec::new() }
    }
    #[inline]
    pub fn clear(&mut self) {
        self.v.clear();
    }
    #[inline]
    pub fn len(&self) -> usize {
        self.v.len()
    }
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.v.is_empty()
    }
    #[inline]
    pub fn push(&mut self, k: u64) {
        let mut i = self.v.len();
        self.v.push(k);
        while i > 0 {
            let p = (i - 1) / 4;
            let pk = self.v[p];
            if pk <= k {
                break;
            }
            self.v[i] = pk;
            i = p;
        }
        self.v[i] = k;
    }
    #[inline]
    pub fn pop(&mut self) -> Option<u64> {
        let last = self.v.pop()?;
        let n = self.v.len();
        if n == 0 {
            return Some(last);
        }
        let top = self.v[0];
        let mut i = 0;
        loop {
            let c = 4 * i + 1;
            if c >= n {
                break;
            }
            let end = (c + 4).min(n);
            let mut m = c;
            let mut mk = self.v[c];
            for j in c + 1..end {
                let x = self.v[j];
                if x < mk {
                    m = j;
                    mk = x;
                }
            }
            if mk >= last {
                break;
            }
            self.v[i] = mk;
            i = m;
        }
        self.v[i] = last;
        Some(top)
    }
}
