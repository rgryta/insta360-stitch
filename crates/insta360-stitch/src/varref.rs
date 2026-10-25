//! The variational refinement of the DIS flow, ported from OpenCV 4.7 (`variational_refinement.cpp`, Apache 2.0):
//! fixed-point iterations of a robust energy (colour and gradient constancy, flow smoothness), each solved with a
//! red-black SOR. OpenCV keeps the red and the black cells in separate buffers; here the cells stay in a plane, with
//! the same order of updates (all red cells, then all black cells).

use crate::dis::{Params, Plane};

const SOR_ITERATIONS: usize = 5;
const OMEGA: f32 = 1.6;
const ZETA: f32 = 0.1;
const EPSILON: f32 = 0.001;

/// OpenCV `remap(INTER_LINEAR, BORDER_REPLICATE)` of a float plane at (x + u, y + v): OpenCV rounds the position
/// to 1/32 pixel.
fn warp(src: &Plane<f32>, u: &Plane<f32>, v: &Plane<f32>) -> Plane<f32> {
    let (w, h) = (src.w, src.h);
    let mut out = Plane::new(w, h);
    let at = |x: isize, y: isize| {
        src.data[y.clamp(0, h as isize - 1) as usize * w + x.clamp(0, w as isize - 1) as usize]
    };
    for y in 0..h {
        for x in 0..w {
            let k = y * w + x;
            let fx = ((x as f32 + u.data[k]) * 32.0).round() as i64;
            let fy = ((y as f32 + v.data[k]) * 32.0).round() as i64;
            let (x0, y0) = ((fx >> 5) as isize, (fy >> 5) as isize);
            let (ax, ay) = ((fx & 31) as f32 / 32.0, (fy & 31) as f32 / 32.0);
            out.data[k] = at(x0, y0) * (1.0 - ax) * (1.0 - ay)
                + at(x0 + 1, y0) * ax * (1.0 - ay)
                + at(x0, y0 + 1) * (1.0 - ax) * ay
                + at(x0 + 1, y0 + 1) * ax * ay;
        }
    }
    out
}

/// OpenCV `Sobel(ksize = 1, BORDER_REPLICATE)`: d/dx = f(x + 1) - f(x - 1) (or d/dy).
fn derivative(f: &Plane<f32>, horizontal: bool) -> Plane<f32> {
    let (w, h) = (f.w, f.h);
    let mut out = Plane::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let (a, b) = if horizontal {
                (
                    f.data[y * w + (x + 1).min(w - 1)],
                    f.data[y * w + x.saturating_sub(1)],
                )
            } else {
                (
                    f.data[(y + 1).min(h - 1) * w + x],
                    f.data[y.saturating_sub(1) * w + x],
                )
            };
            out.data[y * w + x] = a - b;
        }
    }
    out
}

/// Refines the flow (u, v) from `i0` to `i1` in place (`VariationalRefinement::calcUV`).
pub fn refine(p: &Params, i0: &Plane<u8>, i1: &Plane<u8>, u: &mut Plane<f32>, v: &mut Plane<f32>) {
    let (w, h) = (i0.w, i0.h);
    let n = w * h;
    let i0f = Plane {
        w,
        h,
        data: i0.data.iter().map(|&x| f32::from(x)).collect(),
    };
    let i1f = Plane {
        w,
        h,
        data: i1.data.iter().map(|&x| f32::from(x)).collect(),
    };
    let warped = warp(&i1f, u, v);
    let avg = Plane {
        w,
        h,
        data: (0..n)
            .map(|k| i0f.data[k] * 0.5 + warped.data[k] * 0.5)
            .collect(),
    };
    let iz = Plane {
        w,
        h,
        data: (0..n).map(|k| warped.data[k] - i0f.data[k]).collect(),
    };
    let ix = derivative(&avg, true);
    let iy = derivative(&avg, false);
    let ixz = derivative(&iz, true);
    let iyz = derivative(&iz, false);
    let ixx = derivative(&ix, true);
    let ixy = derivative(&ix, false);
    let iyy = derivative(&iy, false);

    let (zeta2, eps2) = (ZETA * ZETA, EPSILON * EPSILON);
    let (alpha2, gamma2, delta2) = (p.alpha / 2.0, p.gamma / 2.0, p.delta / 2.0);
    let (wu, wv) = (u.data.clone(), v.data.clone());
    let (mut du, mut dv) = (vec![0.0f32; n], vec![0.0f32; n]);
    let (mut cu, mut cv) = (wu.clone(), wv.clone());
    let (mut a11, mut a12, mut a22, mut b1, mut b2) = (
        vec![0.0f32; n],
        vec![0.0f32; n],
        vec![0.0f32; n],
        vec![0.0f32; n],
        vec![0.0f32; n],
    );
    let mut weight = vec![0.0f32; n];
    for _ in 0..p.variational_refinement_iter {
        // The data term.
        for k in 0..n {
            let (x, y, z) = (ix.data[k], iy.data[k], iz.data[k]);
            let norm = x * x + y * y + zeta2;
            let ik1z = z + x * du[k] + y * dv[k];
            let wt = (delta2 / (ik1z * ik1z / norm + eps2).sqrt()) / norm;
            a11[k] = wt * (x * x) + zeta2;
            a12[k] = wt * (x * y);
            a22[k] = wt * (y * y) + zeta2;
            b1[k] = -wt * (z * x);
            b2[k] = -wt * (z * y);
            let (xx, xy, yy, xz, yz) = (
                ixx.data[k],
                ixy.data[k],
                iyy.data[k],
                ixz.data[k],
                iyz.data[k],
            );
            let n1 = xx * xx + xy * xy + zeta2;
            let n2 = yy * yy + xy * xy + zeta2;
            let zx = xz + xx * du[k] + xy * dv[k];
            let zy = yz + xy * du[k] + yy * dv[k];
            let wt = gamma2 / (zx * zx / n1 + zy * zy / n2 + eps2).sqrt();
            a11[k] += wt * (xx * xx / n1 + xy * xy / n2);
            a12[k] += wt * (xx * xy / n1 + xy * yy / n2);
            a22[k] += wt * (xy * xy / n1 + yy * yy / n2);
            b1[k] += -wt * (xx * xz / n1 + xy * yz / n2);
            b2[k] += -wt * (xy * xz / n1 + yy * yz / n2);
        }
        // The smoothness term: the weight of each pixel from the flow of this iteration (the next column and row,
        // repeated at the borders), for the edges to its right and down neighbours.
        for y in 0..h {
            for x in 0..w {
                let k = y * w + x;
                let r = y * w + (x + 1).min(w - 1);
                let d = (y + 1).min(h - 1) * w + x;
                let (ux, vx, uy, vy) = (cu[r] - cu[k], cv[r] - cv[k], cu[d] - cu[k], cv[d] - cv[k]);
                weight[k] = alpha2 / (ux * ux + vx * vx + uy * uy + vy * vy + eps2).sqrt();
            }
        }
        for y in 0..h {
            for x in 0..w {
                let k = y * w + x;
                let wt = weight[k];
                if x + 1 < w {
                    let q = k + 1;
                    let (gu, gv) = (wt * (wu[q] - wu[k]), wt * (wv[q] - wv[k]));
                    b1[k] += gu;
                    a11[k] += wt;
                    b2[k] += gv;
                    a22[k] += wt;
                    b1[q] -= gu;
                    a11[q] += wt;
                    b2[q] -= gv;
                    a22[q] += wt;
                }
                if y + 1 < h {
                    let q = k + w;
                    let (gu, gv) = (wt * (wu[q] - wu[k]), wt * (wv[q] - wv[k]));
                    b1[k] += gu;
                    a11[k] += wt;
                    b2[k] += gv;
                    a22[k] += wt;
                    b1[q] -= gu;
                    a11[q] += wt;
                    b2[q] -= gv;
                    a22[q] += wt;
                }
            }
        }
        // Red-black SOR: (x + y) even, then odd.
        for _ in 0..SOR_ITERATIONS {
            for colour in 0..2 {
                for y in 0..h {
                    for x in ((y + colour) % 2..w).step_by(2) {
                        let k = y * w + x;
                        let mut su = 0.0f32;
                        let mut sv = 0.0f32;
                        if x > 0 {
                            su += weight[k - 1] * du[k - 1];
                            sv += weight[k - 1] * dv[k - 1];
                        }
                        if x + 1 < w {
                            su += weight[k] * du[k + 1];
                            sv += weight[k] * dv[k + 1];
                        }
                        if y > 0 {
                            su += weight[k - w] * du[k - w];
                            sv += weight[k - w] * dv[k - w];
                        }
                        if y + 1 < h {
                            su += weight[k] * du[k + w];
                            sv += weight[k] * dv[k + w];
                        }
                        du[k] += OMEGA * ((su + b1[k] - dv[k] * a12[k]) / a11[k] - du[k]);
                        dv[k] += OMEGA * ((sv + b2[k] - du[k] * a12[k]) / a22[k] - dv[k]);
                    }
                }
            }
        }
        for k in 0..n {
            cu[k] = wu[k] + du[k];
            cv[k] = wv[k] + dv[k];
        }
    }
    u.data = cu;
    v.data = cv;
}
