//! This module contains the DIS optical flow (Kroeger et al.), ported from OpenCV 4.7 (`dis_flow.cpp`,
//! `variational_refinement.cpp`, Apache 2.0), with the changes of Insta360 (`bcv::flow::FDSFlow`). Insta360 Studio
//! uses it on the seam strips of a photo (see `seamflow`).
//!
//! Images are 8-bit grey, row by row. A flow is two planes (x, y) of the same size: I0(p) ~ I1(p + flow(p)).

const EPS: f32 = 0.001;
const INF: f32 = 1e10;

/// The parameters (OpenCV `DISOpticalFlow`).
#[derive(Debug, Clone)]
pub struct Params {
    pub finest_scale: usize,
    /// The coarsest scale is the OpenCV formula, at most this value.
    pub max_coarsest_scale: usize,
    pub patch_size: usize,
    pub patch_stride: usize,
    pub grad_descent_iter: usize,
    pub variational_refinement_iter: usize,
    pub alpha: f32,
    pub gamma: f32,
    pub delta: f32,
    pub use_mean_normalization: bool,
    pub use_spatial_propagation: bool,
    /// The number of stripes of the inverse search with spatial propagation (OpenCV: 8; Insta360: 1).
    pub stripes: usize,
}

impl Default for Params {
    /// OpenCV `create(PRESET_MEDIUM)`.
    fn default() -> Self {
        Params {
            finest_scale: 1,
            max_coarsest_scale: usize::MAX,
            patch_size: 8,
            patch_stride: 3,
            grad_descent_iter: 25,
            variational_refinement_iter: 5,
            alpha: 20.0,
            gamma: 10.0,
            delta: 5.0,
            use_mean_normalization: true,
            use_spatial_propagation: true,
            stripes: 8,
        }
    }
}

/// The changes of Insta360 `FDSFlow`: masks, flow limits, weighted rows of low texture and a revert of a bad
/// refinement.
#[derive(Debug, Clone)]
pub struct Fds {
    /// The valid pixels of I0 and I1 (non-zero).
    pub mask0: Plane<u8>,
    pub mask1: Plane<u8>,
    /// The open intervals of the flow (x, y) at the full size. At scale k, Insta360 divides them by 4^k.
    pub limits: [(f32, f32); 2],
}

/// FDS (`updateWeightSSDWorkRows`): the low-texture patch rows of a coarser scale (`s` = 2^(k - finest)). The flagged
/// rows of the finest scale become pixel runs [stride * first, stride * last + psz); each run marks the patch rows
/// from (y / s - psz / 2) / stride to ((y + h) / s - psz / 2) / stride. [inf] The limits of the clamp.
fn level_rows(fine: &[bool], s: f32, psz: usize, stride: usize, hs: usize) -> Vec<bool> {
    let mut out = vec![false; hs];
    let row =
        |y: f32| ((((y / s) - (psz / 2) as f32).max(0.0) / stride as f32) as usize).min(hs - 1);
    let mut is = 0;
    while is < fine.len() {
        if !fine[is] {
            is += 1;
            continue;
        }
        let first = is;
        while is < fine.len() && fine[is] {
            is += 1;
        }
        let (y0, y1) = ((stride * first) as f32, (stride * (is - 1) + psz) as f32);
        out[row(y0)..=row(y1)].iter_mut().for_each(|r| *r = true);
    }
    out
}

/// The mean texture of a patch row below which Insta360 weights the pixels of its patches (`updateRowsLackOfTexture`).
const LOW_TEXTURE: f32 = 2000.0;

/// An image or a plane: `w` x `h` values.
#[derive(Debug, Clone, PartialEq)]
pub struct Plane<T> {
    pub w: usize,
    pub h: usize,
    pub data: Vec<T>,
}

impl<T: Copy + Default> Plane<T> {
    pub fn new(w: usize, h: usize) -> Self {
        Plane {
            w,
            h,
            data: vec![T::default(); w * h],
        }
    }
    #[inline]
    pub fn at(&self, x: usize, y: usize) -> T {
        self.data[y * self.w + x]
    }
}

/// OpenCV `resize(INTER_AREA)` by exactly 1/2 (even sizes): the rounded mean of each 2 x 2 block.
fn half(src: &Plane<u8>) -> Plane<u8> {
    let (w, h) = (src.w / 2, src.h / 2);
    let mut out = Plane::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let s = u32::from(src.at(2 * x, 2 * y))
                + u32::from(src.at(2 * x + 1, 2 * y))
                + u32::from(src.at(2 * x, 2 * y + 1))
                + u32::from(src.at(2 * x + 1, 2 * y + 1));
            out.data[y * w + x] = ((s + 2) >> 2) as u8;
        }
    }
    out
}

/// A border index for OpenCV BORDER_REFLECT_101.
#[inline]
fn reflect101(i: isize, n: usize) -> usize {
    let n = n as isize;
    if n == 1 {
        return 0;
    }
    let mut i = i;
    while i < 0 || i >= n {
        i = if i < 0 { -i } else { 2 * (n - 1) - i };
    }
    i as usize
}

/// OpenCV `spatialGradient` (3 x 3 Sobel, BORDER_REFLECT_101) as i16.
fn spatial_gradient(img: &Plane<u8>) -> (Plane<i16>, Plane<i16>) {
    let (w, h) = (img.w, img.h);
    let mut gx = Plane::new(w, h);
    let mut gy = Plane::new(w, h);
    let at = |x: isize, y: isize| i32::from(img.at(reflect101(x, w), reflect101(y, h)));
    for y in 0..h as isize {
        for x in 0..w as isize {
            let dx = (at(x + 1, y - 1) - at(x - 1, y - 1))
                + 2 * (at(x + 1, y) - at(x - 1, y))
                + (at(x + 1, y + 1) - at(x - 1, y + 1));
            let dy = (at(x - 1, y + 1) - at(x - 1, y - 1))
                + 2 * (at(x, y + 1) - at(x, y - 1))
                + (at(x + 1, y + 1) - at(x + 1, y - 1));
            gx.data[y as usize * w + x as usize] = dx as i16;
            gy.data[y as usize * w + x as usize] = dy as i16;
        }
    }
    (gx, gy)
}

/// I1 with a border of `b` pixels (BORDER_REPLICATE).
fn extend(img: &Plane<u8>, b: usize) -> Plane<u8> {
    let (w, h) = (img.w + 2 * b, img.h + 2 * b);
    let mut out = Plane::new(w, h);
    for y in 0..h {
        let sy = (y as isize - b as isize).clamp(0, img.h as isize - 1) as usize;
        for x in 0..w {
            let sx = (x as isize - b as isize).clamp(0, img.w as isize - 1) as usize;
            out.data[y * w + x] = img.at(sx, sy);
        }
    }
    out
}

/// OpenCV `resize(INTER_LINEAR)` of a float plane to `w` x `h` (half-pixel centres, borders clamped).
pub fn resize_linear(src: &Plane<f32>, w: usize, h: usize) -> Plane<f32> {
    let mut out = Plane::new(w, h);
    let (sx, sy) = (src.w as f64 / w as f64, src.h as f64 / h as f64);
    let coord = |d: usize, scale: f64, n: usize| -> (usize, usize, f32) {
        let f = ((d as f64 + 0.5) * scale - 0.5) as f32;
        let i = f.floor();
        let mut t = f - i;
        let mut i = i as isize;
        if i < 0 {
            i = 0;
            t = 0.0;
        }
        if i as usize >= n - 1 {
            return (n - 1, n - 1, 0.0);
        }
        (i as usize, i as usize + 1, t)
    };
    for y in 0..h {
        let (y0, y1, ty) = coord(y, sy, src.h);
        for x in 0..w {
            let (x0, x1, tx) = coord(x, sx, src.w);
            let top = src.at(x0, y0) * (1.0 - tx) + src.at(x1, y0) * tx;
            let bottom = src.at(x0, y1) * (1.0 - tx) + src.at(x1, y1) * tx;
            out.data[y * w + x] = top * (1.0 - ty) + bottom * ty;
        }
    }
    out
}

/// The buffers of one scale. With FDS: the masks (I1 with the border), and the texture weight of each I0 pixel.
struct Scale {
    i0: Plane<u8>,
    i1: Plane<u8>,
    i1_ext: Plane<u8>,
    i0x: Plane<i16>,
    i0y: Plane<i16>,
    m0: Option<Plane<u8>>,
    m1_ext: Option<Plane<u8>>,
    weight: Option<Plane<f32>>,
}

/// OpenCV `resize(INTER_NEAREST)` by 1/2: the top-left pixel of each 2 x 2 block.
fn half_nearest(src: &Plane<u8>) -> Plane<u8> {
    let (w, h) = (src.w / 2, src.h / 2);
    Plane {
        w,
        h,
        data: (0..w * h)
            .map(|k| src.at(2 * (k % w), 2 * (k / w)))
            .collect(),
    }
}

/// OpenCV `GaussianBlur(ksize x ksize, sigma, BORDER_REFLECT_101)` of a float plane (`getGaussianKernel` with
/// sigma > 0).
pub(crate) fn gaussian_blur(src: &Plane<f32>, ksize: usize, sigma: f64) -> Plane<f32> {
    let r = (ksize / 2) as isize;
    let g: Vec<f64> = (-r..=r)
        .map(|i| (-((i * i) as f64) / (2.0 * sigma * sigma)).exp())
        .collect();
    let sum: f64 = g.iter().sum();
    let k: Vec<f32> = g.iter().map(|v| (v / sum) as f32).collect();
    let (w, h) = (src.w, src.h);
    let mut tmp: Plane<f32> = Plane::new(w, h);
    for y in 0..h {
        for x in 0..w {
            tmp.data[y * w + x] = (0..ksize)
                .map(|i| k[i] * src.at(reflect101(x as isize + i as isize - r, w), y))
                .sum();
        }
    }
    let mut out = Plane::new(w, h);
    for y in 0..h {
        for x in 0..w {
            out.data[y * w + x] = (0..ksize)
                .map(|i| k[i] * tmp.at(x, reflect101(y as isize + i as isize - r, h)))
                .sum();
        }
    }
    out
}

/// The FDS buffers of a scale: I0x, I0y are 0 outside mask 0; the weight is the blurred |I0x| + |I0y| inside it.
fn fds_scale(s: &mut Scale, m0: Plane<u8>, m1: &Plane<u8>) {
    for k in 0..m0.data.len() {
        if m0.data[k] == 0 {
            s.i0x.data[k] = 0;
            s.i0y.data[k] = 0;
        }
    }
    let grad = Plane {
        w: m0.w,
        h: m0.h,
        data: (0..m0.data.len())
            .map(|k| {
                f32::from(s.i0x.data[k].unsigned_abs()) + f32::from(s.i0y.data[k].unsigned_abs())
            })
            .collect(),
    };
    let mut weight = gaussian_blur(&grad, 3, 1.0);
    for k in 0..m0.data.len() {
        if m0.data[k] == 0 {
            weight.data[k] = 0.0;
        }
    }
    s.weight = Some(weight);
    s.m1_ext = Some(extend(m1, BORDER));
    s.m0 = Some(m0);
}

/// The structure tensor and gradient sums of each patch of the sparse grid (`precomputeStructureTensor`).
struct Tensor {
    xx: Vec<f32>,
    yy: Vec<f32>,
    xy: Vec<f32>,
    x: Vec<f32>,
    y: Vec<f32>,
    /// FDS: the sum of the texture weight over each patch.
    t: Vec<f32>,
}

/// The sum of `values` over each patch of the sparse grid (the box sums of `precomputeStructureTensor`).
fn patch_sums(values: &Plane<f32>, psz: usize, stride: usize, ws: usize, hs: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; ws * hs];
    for is in 0..hs {
        for js in 0..ws {
            let (i, j) = (is * stride, js * stride);
            out[is * ws + js] = (0..psz)
                .flat_map(|a| (0..psz).map(move |b| (a, b)))
                .map(|(a, b)| values.at(j + b, i + a))
                .sum();
        }
    }
    out
}

fn structure_tensor(s: &Scale, psz: usize, stride: usize, ws: usize, hs: usize) -> Tensor {
    let (w, h) = (s.i0.w, s.i0.h);
    let mut aux: [Vec<f32>; 5] = std::array::from_fn(|_| vec![0.0; h * ws]);
    for i in 0..h {
        let xr = &s.i0x.data[i * w..(i + 1) * w];
        let yr = &s.i0y.data[i * w..(i + 1) * w];
        let v = |j: usize| (f32::from(xr[j]), f32::from(yr[j]));
        let (mut sxx, mut syy, mut sxy, mut sx, mut sy) = (0.0f32, 0.0f32, 0.0f32, 0.0f32, 0.0f32);
        for j in 0..psz {
            let (a, b) = v(j);
            sxx += a * a;
            syy += b * b;
            sxy += a * b;
            sx += a;
            sy += b;
        }
        let store = |aux: &mut [Vec<f32>; 5], js: usize, vals: [f32; 5]| {
            for k in 0..5 {
                aux[k][i * ws + js] = vals[k];
            }
        };
        store(&mut aux, 0, [sxx, syy, sxy, sx, sy]);
        let mut js = 1;
        for j in psz..w {
            let ((a, b), (c, d)) = (v(j), v(j - psz));
            sxx += a * a - c * c;
            syy += b * b - d * d;
            sxy += a * b - c * d;
            sx += a - c;
            sy += b - d;
            if (j - psz + 1).is_multiple_of(stride) && js < ws {
                store(&mut aux, js, [sxx, syy, sxy, sx, sy]);
                js += 1;
            }
        }
    }
    let mut out: [Vec<f32>; 5] = std::array::from_fn(|_| vec![0.0; hs * ws]);
    let mut sums = [
        vec![0.0f32; ws],
        vec![0.0; ws],
        vec![0.0; ws],
        vec![0.0; ws],
        vec![0.0; ws],
    ];
    for i in 0..psz {
        for j in 0..ws {
            for k in 0..5 {
                sums[k][j] += aux[k][i * ws + j];
            }
        }
    }
    for j in 0..ws {
        for k in 0..5 {
            out[k][j] = sums[k][j];
        }
    }
    let mut is = 1;
    for i in psz..h {
        for j in 0..ws {
            for k in 0..5 {
                sums[k][j] += aux[k][i * ws + j] - aux[k][(i - psz) * ws + j];
            }
        }
        if (i - psz + 1).is_multiple_of(stride) && is < hs {
            for j in 0..ws {
                for k in 0..5 {
                    out[k][is * ws + j] = sums[k][j];
                }
            }
            is += 1;
        }
    }
    let [xx, yy, xy, x, y] = out;
    let t = s
        .weight
        .as_ref()
        .map_or_else(Vec::new, |w| patch_sums(w, psz, stride, ws, hs));
    Tensor {
        xx,
        yy,
        xy,
        x,
        y,
        t,
    }
}

/// The patch of `psz` x `psz` at I0 (i, j) against I1_ext at the bilinear position, as `processPatch*` /
/// `computeSSD*`. Gives (SSD, dUx, dUy); with `grad == false` only the SSD.
#[allow(clippy::too_many_arguments)]
#[inline]
fn patch(
    s: &Scale,
    i: usize,
    j: usize,
    i1: (usize, usize),
    w: [f32; 4],
    psz: usize,
    mean_norm: bool,
    grad: Option<(f32, f32)>,
    row_weight: Option<f32>,
) -> (f32, f32, f32) {
    let (w0, wx) = (s.i0.w, s.i1_ext.w);
    if let (Some(m0), Some(m1)) = (&s.m0, &s.m1_ext) {
        // FDS: only the pixels valid in I0 and at the integer corner in I1; in a weighted row, each difference is
        // multiplied by weight(pixel) / sum of the weights of the patch.
        let (mut ssd, mut sum, mut ux, mut uy, mut n) = (0.0f32, 0.0f32, 0.0f32, 0.0f32, 0usize);
        for a in 0..psz {
            let r0 = (i + a) * w0 + j;
            let r1 = (i1.0 + a) * wx + i1.1;
            for b in 0..psz {
                if m0.data[r0 + b] == 0 || m1.data[r1 + b] == 0 {
                    continue;
                }
                let mut d = w[0] * f32::from(s.i1_ext.data[r1 + b])
                    + w[1] * f32::from(s.i1_ext.data[r1 + b + 1])
                    + w[2] * f32::from(s.i1_ext.data[r1 + wx + b])
                    + w[3] * f32::from(s.i1_ext.data[r1 + wx + b + 1])
                    - f32::from(s.i0.data[r0 + b]);
                if let (Some(rw), Some(wt)) = (row_weight, &s.weight) {
                    d *= wt.data[r0 + b] * rw;
                }
                ssd += d * d;
                sum += d;
                n += 1;
                if grad.is_some() {
                    ux += d * f32::from(s.i0x.data[r0 + b]);
                    uy += d * f32::from(s.i0y.data[r0 + b]);
                }
            }
        }
        if n <= 8 {
            return (INF, 0.0, 0.0);
        }
        let n = n as f32;
        return match (mean_norm, grad) {
            (true, Some((gx, gy))) => (ssd - sum * sum / n, ux - sum * gx / n, uy - sum * gy / n),
            (true, None) => (ssd - sum * sum / n, 0.0, 0.0),
            (false, _) => (ssd, ux, uy),
        };
    }
    let diff = |r0: usize, r1: usize, b: usize| {
        w[0] * f32::from(s.i1_ext.data[r1 + b])
            + w[1] * f32::from(s.i1_ext.data[r1 + b + 1])
            + w[2] * f32::from(s.i1_ext.data[r1 + wx + b])
            + w[3] * f32::from(s.i1_ext.data[r1 + wx + b + 1])
            - f32::from(s.i0.data[r0 + b])
    };
    let (mut ssd, mut sum, mut ux, mut uy) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
    if psz == 8 {
        // As the 128-bit SIMD path of OpenCV: lane l sums the columns l and l + 4; then (l0 + l2) + (l1 + l3).
        let (mut lssd, mut lsum, mut lux, mut luy) = ([0f32; 4], [0f32; 4], [0f32; 4], [0f32; 4]);
        for a in 0..8 {
            let r0 = (i + a) * w0 + j;
            let r1 = (i1.0 + a) * wx + i1.1;
            for l in 0..4 {
                let (dl, dr) = (diff(r0, r1, l), diff(r0, r1, l + 4));
                lssd[l] += dl * dl + dr * dr;
                lsum[l] += dl + dr;
                if grad.is_some() {
                    lux[l] +=
                        dl * f32::from(s.i0x.data[r0 + l]) + dr * f32::from(s.i0x.data[r0 + l + 4]);
                    luy[l] +=
                        dl * f32::from(s.i0y.data[r0 + l]) + dr * f32::from(s.i0y.data[r0 + l + 4]);
                }
            }
        }
        let reduce = |v: [f32; 4]| (v[0] + v[2]) + (v[1] + v[3]);
        (ssd, sum, ux, uy) = (reduce(lssd), reduce(lsum), reduce(lux), reduce(luy));
    } else {
        for a in 0..psz {
            let r0 = (i + a) * w0 + j;
            let r1 = (i1.0 + a) * wx + i1.1;
            for b in 0..psz {
                let d = diff(r0, r1, b);
                ssd += d * d;
                sum += d;
                if grad.is_some() {
                    ux += d * f32::from(s.i0x.data[r0 + b]);
                    uy += d * f32::from(s.i0y.data[r0 + b]);
                }
            }
        }
    }
    let n = (psz * psz) as f32;
    match (mean_norm, grad) {
        (true, Some((gx, gy))) => (ssd - sum * sum / n, ux - sum * gx / n, uy - sum * gy / n),
        (true, None) => (ssd - sum * sum / n, 0.0, 0.0),
        (false, _) => (ssd, ux, uy),
    }
}

struct Sparse {
    sx: Vec<f32>,
    sy: Vec<f32>,
}

/// `PatchInverseSearch_ParBody` for all stripes.
#[allow(clippy::too_many_arguments)]
fn inverse_search(
    p: &Params,
    s: &Scale,
    t: &Tensor,
    ux: &Plane<f32>,
    uy: &Plane<f32>,
    initial: Option<&(Plane<f32>, Plane<f32>)>,
    ws: usize,
    hs: usize,
    sp: &mut Sparse,
    limits: Option<[(f32, f32); 2]>,
    weighted_rows: &[bool],
) {
    let (psz, stride) = (p.patch_size, p.patch_stride);
    let (w, h) = (s.i0.w, s.i0.h);
    let bsz = BORDER;
    let num_iter = if p.use_spatial_propagation { 2 } else { 1 };
    let stripes = if p.use_spatial_propagation {
        p.stripes
    } else {
        1
    };
    let stripe_sz = (hs as f64 / stripes as f64).ceil() as usize;
    let (il, iu) = (bsz as f32 - psz as f32 + 1.0, bsz as f32 + h as f32 - 1.0);
    let (jl, ju) = (bsz as f32 - psz as f32 + 1.0, bsz as f32 + w as f32 - 1.0);
    let weights = |i: usize, j: usize, fx: f32, fy: f32| {
        let i_i1 = (i as f32 + fy + bsz as f32).max(il).min(iu);
        let j_i1 = (j as f32 + fx + bsz as f32).max(jl).min(ju);
        let (di, dj) = (i_i1 - i_i1.floor(), j_i1 - j_i1.floor());
        (
            (i_i1 as usize, j_i1 as usize),
            [
                (1.0 - di) * (1.0 - dj),
                (1.0 - di) * dj,
                di * (1.0 - dj),
                di * dj,
            ],
        )
    };
    // FDS: in a row of low texture, the pixel weights of each patch (1 / sum, 0 for a sum <= 0).
    let row_weight = |is: usize, k: usize| -> Option<f32> {
        (weighted_rows.get(is).copied().unwrap_or(false)).then(|| {
            if t.t[k] > 0.0 {
                1.0 / t.t[k]
            } else {
                0.0
            }
        })
    };
    let ssd = |i: usize, j: usize, fx: f32, fy: f32, rw: Option<f32>| {
        let (pos, wts) = weights(i, j, fx, fy);
        patch(s, i, j, pos, wts, psz, p.use_mean_normalization, None, rw).0
    };
    let inner = (p.grad_descent_iter as f32 / num_iter as f32).floor() as usize;
    let psz2 = psz / 2;
    for stripe in 0..stripes {
        for iter in 0..num_iter {
            let (dir, start_is, end_is, start_js, end_js): (isize, isize, isize, isize, isize) =
                if iter % 2 == 0 {
                    (
                        1,
                        (stripe * stripe_sz).min(hs) as isize,
                        ((stripe + 1) * stripe_sz).min(hs) as isize,
                        0,
                        ws as isize,
                    )
                } else {
                    (
                        -1,
                        ((stripe + 1) * stripe_sz).min(hs) as isize - 1,
                        (stripe * stripe_sz).min(hs) as isize - 1,
                        ws as isize - 1,
                        -1,
                    )
                };
            let mut is = start_is;
            while dir * is < dir * end_is {
                let i = is as usize * stride;
                let mut js = start_js;
                while dir * js < dir * end_js {
                    let j = js as usize * stride;
                    let k = is as usize * ws + js as usize;
                    let rw = row_weight(is as usize, k);
                    if iter == 0 {
                        sp.sx[k] = ux.at(j + psz2, i + psz2);
                        sp.sy[k] = uy.at(j + psz2, i + psz2);
                    }
                    let mut min_ssd = INF;
                    if initial.is_some() || p.use_spatial_propagation {
                        min_ssd = ssd(i, j, sp.sx[k], sp.sy[k], rw);
                    }
                    if let Some((ix, iy)) = initial {
                        let (cx, cy) = (ix.at(j + psz2, i + psz2), iy.at(j + psz2, i + psz2));
                        let c = ssd(i, j, cx, cy, rw);
                        if c < min_ssd {
                            min_ssd = c;
                            sp.sx[k] = cx;
                            sp.sy[k] = cy;
                        }
                    }
                    if p.use_spatial_propagation {
                        if dir * js > dir * start_js {
                            let n = (is * ws as isize + js - dir) as usize;
                            let c = ssd(i, j, sp.sx[n], sp.sy[n], rw);
                            if c < min_ssd {
                                min_ssd = c;
                                sp.sx[k] = sp.sx[n];
                                sp.sy[k] = sp.sy[n];
                            }
                        }
                        if dir * is > dir * start_is {
                            let n = ((is - dir) * ws as isize + js) as usize;
                            let c = ssd(i, j, sp.sx[n], sp.sy[n], rw);
                            if c < min_ssd {
                                sp.sx[k] = sp.sx[n];
                                sp.sy[k] = sp.sy[n];
                            }
                        }
                    }
                    let _ = min_ssd;
                    let (mut cx, mut cy) = (sp.sx[k], sp.sy[k]);
                    let mut det = t.xx[k] * t.yy[k] - t.xy[k] * t.xy[k];
                    if det.abs() < EPS {
                        det = EPS;
                    }
                    let (h11, h12, h22) = (t.yy[k] / det, -t.xy[k] / det, t.xx[k] / det);
                    let mut prev = INF;
                    for _ in 0..inner {
                        let (pos, wts) = weights(i, j, cx, cy);
                        let (cur, dux, duy) = patch(
                            s,
                            i,
                            j,
                            pos,
                            wts,
                            psz,
                            p.use_mean_normalization,
                            Some((t.x[k], t.y[k])),
                            rw,
                        );
                        cx -= h11 * dux + h12 * duy;
                        cy -= h12 * dux + h22 * duy;
                        if cur >= prev {
                            break;
                        }
                        prev = cur;
                    }
                    // FDS: the new flow must also be strictly inside the limits of this scale.
                    let inside = |v: f32, (a, b): (f32, f32)| v > a.min(b) && v < a.max(b);
                    let allowed = limits.is_none_or(|[lx, ly]| inside(cx, lx) && inside(cy, ly));
                    if (cx - sp.sx[k]).hypot(cy - sp.sy[k]) <= psz as f32 && allowed {
                        sp.sx[k] = cx;
                        sp.sy[k] = cy;
                    }
                    js += dir;
                }
                is += dir;
            }
        }
    }
}

/// `Densification_ParBody`: the dense flow from the overlapping patches, weighted by 1 / max(1, |I1(p + s) - I0(p)|).
fn densify(
    p: &Params,
    s: &Scale,
    sp: &Sparse,
    ws: usize,
    ux: &mut Plane<f32>,
    uy: &mut Plane<f32>,
) {
    let (psz, stride) = (p.patch_size as isize, p.patch_stride as isize);
    let (w, h) = (s.i0.w as isize, s.i0.h as isize);
    let (mut start_is, mut end_is) = (0isize, -1isize);
    for i in 0..h {
        if i % stride == 0 && i + psz <= h {
            end_is += 1;
        }
        if i - psz >= 0 && (i - psz) % stride == 0 && start_is < end_is {
            start_is += 1;
        }
        let (mut start_js, mut end_js) = (0isize, -1isize);
        for j in 0..w {
            if j % stride == 0 && j + psz <= w {
                end_js += 1;
            }
            if j - psz >= 0 && (j - psz) % stride == 0 && start_js < end_js {
                start_js += 1;
            }
            let (mut sum, mut sx, mut sy) = (0.0f32, 0.0f32, 0.0f32);
            for is in start_is..=end_is {
                for js in start_js..=end_js {
                    let k = (is * ws as isize + js) as usize;
                    let jm = (j as f32 + sp.sx[k]).max(0.0).min(w as f32 - 1.0 - EPS);
                    let im = (i as f32 + sp.sy[k]).max(0.0).min(h as f32 - 1.0 - EPS);
                    let (jl, il) = (jm as usize, im as usize);
                    let (ju, iu) = (jl + 1, il + 1);
                    let px = |x: usize, y: usize| f32::from(s.i1.at(x, y));
                    let diff = (jm - jl as f32) * (im - il as f32) * px(ju, iu)
                        + (ju as f32 - jm) * (im - il as f32) * px(jl, iu)
                        + (jm - jl as f32) * (iu as f32 - im) * px(ju, il)
                        + (ju as f32 - jm) * (iu as f32 - im) * px(jl, il)
                        - f32::from(s.i0.at(j as usize, i as usize));
                    let c = 1.0 / diff.abs().max(1.0);
                    sx += c * sp.sx[k];
                    sy += c * sp.sy[k];
                    sum += c;
                }
            }
            ux.data[(i * w + j) as usize] = sx / sum;
            uy.data[(i * w + j) as usize] = sy / sum;
        }
    }
}

const BORDER: usize = 16;

/// The DIS flow from `i0` to `i1`. `init` is the initial flow (OpenCV `use_input_flow`: only a candidate of the
/// coarsest scale is not used; the initial flow is a temporal candidate on all scales).
pub fn dis(
    p: &Params,
    i0: &Plane<u8>,
    i1: &Plane<u8>,
    init: Option<&(Plane<f32>, Plane<f32>)>,
    fds: Option<&Fds>,
) -> (Plane<f32>, Plane<f32>) {
    let (cols, rows) = (i0.w, i0.h);
    let coarsest = (((cols.max(rows) as f64 / (4.0 * p.patch_size as f64)).ln() / 2f64.ln() + 0.5)
        as usize)
        .min(((cols.min(rows) / p.patch_size) as f64).log2() as usize)
        .min(p.max_coarsest_scale);
    assert!(
        coarsest >= p.finest_scale,
        "image too small for the finest scale"
    );
    // The pyramid: scale k is the image reduced by 2^k (INTER_AREA). FDS also reduces the masks (INTER_NEAREST).
    let mut levels: Vec<(Plane<u8>, Plane<u8>)> = vec![(i0.clone(), i1.clone())];
    let mut masks: Vec<(Plane<u8>, Plane<u8>)> = fds
        .map(|f| vec![(f.mask0.clone(), f.mask1.clone())])
        .unwrap_or_default();
    for k in 1..=coarsest {
        let (a, b) = &levels[k - 1];
        levels.push((half(a), half(b)));
        if let Some((a, b)) = masks.last() {
            let next = (half_nearest(a), half_nearest(b));
            masks.push(next);
        }
    }
    let grid = |w: usize, h: usize| {
        (
            1 + (w - p.patch_size) / p.patch_stride,
            1 + (h - p.patch_size) / p.patch_stride,
        )
    };
    // The buffers and the structure tensors of all scales (FDS needs the texture of the finest scale first).
    let scales: Vec<(Scale, Tensor)> = (p.finest_scale..=coarsest)
        .map(|k| {
            let (a, b) = &levels[k];
            let (gx, gy) = spatial_gradient(a);
            let mut s = Scale {
                i0: a.clone(),
                i1: b.clone(),
                i1_ext: extend(b, BORDER),
                i0x: gx,
                i0y: gy,
                m0: None,
                m1_ext: None,
                weight: None,
            };
            if let Some((m0, m1)) = masks.get(k) {
                fds_scale(&mut s, m0.clone(), m1);
            }
            let (ws, hs) = grid(a.w, a.h);
            let t = structure_tensor(&s, p.patch_size, p.patch_stride, ws, hs);
            (s, t)
        })
        .collect();
    // FDS (`updateRowsLackOfTexture`): a patch row of the finest scale has low texture when the mean T of its
    // patches with T > 1 is less than LOW_TEXTURE (a row without such a patch has the mean 0).
    let low_texture: Vec<bool> = match fds {
        None => Vec::new(),
        Some(_) => {
            let (s, t) = &scales[0];
            let (ws, hs) = grid(s.i0.w, s.i0.h);
            (0..hs)
                .map(|is| {
                    let rich: Vec<f32> = t.t[is * ws..(is + 1) * ws]
                        .iter()
                        .copied()
                        .filter(|&v| v > 1.0)
                        .collect();
                    let mean = if rich.is_empty() {
                        0.0
                    } else {
                        rich.iter().sum::<f32>() / rich.len() as f32
                    };
                    mean < LOW_TEXTURE
                })
                .collect()
        }
    };
    let mut flow: Option<(Plane<f32>, Plane<f32>)> = None;
    for k in (p.finest_scale..=coarsest).rev() {
        let (s, t) = &scales[k - p.finest_scale];
        let (w, h) = (s.i0.w, s.i0.h);
        let (ux, uy) = flow
            .take()
            .unwrap_or_else(|| (Plane::new(w, h), Plane::new(w, h)));
        let (ws, hs) = grid(w, h);
        let mut sp = Sparse {
            sx: vec![0.0; ws * hs],
            sy: vec![0.0; ws * hs],
        };
        // The initial flow at this scale (OpenCV: resized, divided by 2^k), a candidate of each patch.
        let initial = init.map(|(x, y)| {
            let f = (1usize << k) as f32;
            let mut a = resize_linear(x, w, h);
            let mut b = resize_linear(y, w, h);
            a.data.iter_mut().for_each(|v| *v /= f);
            b.data.iter_mut().for_each(|v| *v /= f);
            (a, b)
        });
        let limits = fds.map(|f| {
            let d = (1usize << (2 * k)) as f32;
            f.limits.map(|(a, b)| (a / d, b / d))
        });
        let rows = if k == p.finest_scale {
            low_texture.clone()
        } else {
            level_rows(
                &low_texture,
                (1usize << (k - p.finest_scale)) as f32,
                p.patch_size,
                p.patch_stride,
                hs,
            )
        };
        inverse_search(
            p,
            s,
            t,
            &ux,
            &uy,
            initial.as_ref(),
            ws,
            hs,
            &mut sp,
            limits,
            &rows,
        );
        let (mut ux, mut uy) = (ux, uy);
        densify(p, s, &sp, ws, &mut ux, &mut uy);
        // FDS refines only the two finest scales, and reverts a refinement that gives NaN or a flow wider than the
        // scale.
        if p.variational_refinement_iter > 0 && (fds.is_none() || k <= p.finest_scale + 1) {
            let before = fds.map(|_| (ux.clone(), uy.clone()));
            crate::varref::refine(p, &s.i0, &s.i1, &mut ux, &mut uy);
            if let Some((bx, by)) = before {
                let bad = |v: &f32| v.is_nan() || *v > w as f32;
                if ux.data.iter().any(bad) || uy.data.iter().any(bad) {
                    (ux, uy) = (bx, by);
                }
            }
        }
        if k > p.finest_scale {
            let (nw, nh) = (levels[k - 1].0.w, levels[k - 1].0.h);
            let mut nx = resize_linear(&ux, nw, nh);
            let mut ny = resize_linear(&uy, nw, nh);
            nx.data.iter_mut().for_each(|v| *v *= 2.0);
            ny.data.iter_mut().for_each(|v| *v *= 2.0);
            flow = Some((nx, ny));
        } else {
            flow = Some((ux, uy));
        }
    }
    let (ux, uy) = flow.unwrap();
    let scale = (1 << p.finest_scale) as f32;
    let mut fx = resize_linear(&ux, cols, rows);
    let mut fy = resize_linear(&uy, cols, rows);
    fx.data.iter_mut().for_each(|v| *v *= scale);
    fy.data.iter_mut().for_each(|v| *v *= scale);
    (fx, fy)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A textured image of `w` x `h`, moved right by `dx` and down by `dy` pixels.
    fn texture(w: usize, h: usize, dx: f64, dy: f64) -> Plane<u8> {
        let value = |x: f64, y: f64| {
            128.0
                + 40.0 * (x * 0.31 + y * 0.07).sin()
                + 35.0 * (y * 0.23 - x * 0.11).cos()
                + 25.0 * (x * 0.53).sin() * (y * 0.41).cos()
        };
        Plane {
            w,
            h,
            data: (0..w * h)
                .map(|k| value((k % w) as f64 - dx, (k / w) as f64 - dy).round() as u8)
                .collect(),
        }
    }

    /// The mean flow of the centre of the image (a border of 16 pixels is not used).
    fn centre_mean(f: &(Plane<f32>, Plane<f32>)) -> (f32, f32) {
        let (w, h) = (f.0.w, f.0.h);
        let ks: Vec<usize> = (16..h - 16)
            .flat_map(|y| (16..w - 16).map(move |x| y * w + x))
            .collect();
        let n = ks.len() as f32;
        (
            ks.iter().map(|&k| f.0.data[k]).sum::<f32>() / n,
            ks.iter().map(|&k| f.1.data[k]).sum::<f32>() / n,
        )
    }

    fn params() -> Params {
        Params {
            grad_descent_iter: 12,
            max_coarsest_scale: 2,
            stripes: 1,
            ..Params::default()
        }
    }

    fn fds(w: usize, h: usize, limits: [(f32, f32); 2]) -> Fds {
        let all = Plane {
            w,
            h,
            data: vec![255; w * h],
        };
        Fds {
            mask0: all.clone(),
            mask1: all,
            limits,
        }
    }

    #[test]
    fn dis_finds_a_translation() {
        // I0(p) = I1(p + flow): the content of I1 is 3 pixels to the right and 1 down.
        let (i0, i1) = (texture(96, 96, 0.0, 0.0), texture(96, 96, 3.0, 1.0));
        let (fx, fy) = centre_mean(&dis(&params(), &i0, &i1, None, None));
        assert!(
            (fx - 3.0).abs() < 0.2 && (fy - 1.0).abs() < 0.2,
            "{fx} {fy}"
        );
        // The same with the FDS changes and limits that include the motion.
        let f = fds(96, 96, [(-4.0, 60.0), (-2.0, 2.0)]);
        let (fx, fy) = centre_mean(&dis(&params(), &i0, &i1, None, Some(&f)));
        assert!(
            (fx - 3.0).abs() < 0.2 && (fy - 1.0).abs() < 0.2,
            "{fx} {fy}"
        );
    }

    #[test]
    fn fds_limits_refuse_a_flow_outside_them() {
        // A motion of 3 pixels to the right with the L2R limits of the X5, which let only 4 / 4^k pixels of scale k
        // to the right.
        let (i0, i1) = (texture(96, 96, 0.0, 0.0), texture(96, 96, 3.0, 0.0));
        let p = Params {
            variational_refinement_iter: 0,
            ..params()
        };
        let f = fds(96, 96, [(-60.0, 4.0), (-2.0, 2.0)]);
        let (fx, _) = centre_mean(&dis(&p, &i0, &i1, None, Some(&f)));
        let (free, _) = centre_mean(&dis(&p, &i0, &i1, None, None));
        assert!((free - 3.0).abs() < 0.3, "{free}");
        // At the finest scale (1), the limit is 4 / 4 = 1 pixel of that scale: 2 pixels of the image.
        assert!(fx < 2.0, "{fx}");
    }

    #[test]
    fn fds_patches_with_8_valid_pixels_or_fewer_do_not_move() {
        // Only one column of I0 is valid: each patch has at most 8 valid pixels, thus no cost, and the flow stays 0.
        let (i0, i1) = (texture(96, 96, 0.0, 0.0), texture(96, 96, 2.0, 0.0));
        let p = Params {
            variational_refinement_iter: 0,
            ..params()
        };
        let mut f = fds(96, 96, [(-60.0, 60.0), (-60.0, 60.0)]);
        for (k, m) in f.mask0.data.iter_mut().enumerate() {
            *m = if k % 96 == 40 { 255 } else { 0 };
        }
        let (fx, fy) = dis(&p, &i0, &i1, None, Some(&f));
        assert!(fx.data.iter().chain(&fy.data).all(|&v| v == 0.0));
    }

    #[test]
    fn low_texture_rows_map_to_the_patch_rows_of_a_coarser_scale() {
        // Rows 10-12 of the finest scale are pixels 30..44; at half size, patch rows (15 - 4) / 3 = 3 to
        // (22 - 4) / 3 = 6.
        let mut fine = vec![false; 40];
        fine[10..=12].iter_mut().for_each(|r| *r = true);
        let rows = level_rows(&fine, 2.0, 8, 3, 20);
        let set: Vec<usize> = (0..20).filter(|&r| rows[r]).collect();
        assert_eq!(set, vec![3, 4, 5, 6]);
        // A run at the top clamps at row 0.
        let rows = level_rows(&[true, false], 2.0, 8, 3, 5);
        assert_eq!(rows, vec![true, false, false, false, false]);
    }
}
