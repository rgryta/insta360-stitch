//! This module matches the colours of the two lenses at the seam, as Insta360 Studio ("Chromatic Calibration",
//! `FisheyeImageFusion` with the MGP algorithm).
//!
//! The module samples a band of 4 rows around the seam from each lens, on a sphere whose equator is the seam
//! (200 x 100). It finds the pixels where the two lenses show the same structure (normalized cross-correlation),
//! and solves for a colour correction of each lens (Y, U and V, least squares with a smoothness term, conjugate
//! gradient). The result is a gain map for each lens on a 200 x 100 panorama. The render multiplies each lens
//! colour c (0 to 255) before the seam blend: c' = (c + 255) * gain - 255. Away from the seam, the gain is 1.
//! The panorama has the camera frame: lens 0 at its centre direction, the zenith at the top.

use crate::blend::{camera_from_pano, pano_from_camera};
use crate::lens::LensModel;
use crate::math::*;

const W: usize = 200;
const H: usize = 100;
/// The rows of the seam sphere that Insta360 samples: within 2 % of H of the equator.
const BAND: [usize; 4] = [48, 49, 50, 51];
/// Insta360 samples each band at 800 x 16, then reduces it to 200 x 4 (`INTER_AREA`).
const BAND_SAMPLES: (usize, usize) = (800, 16);
/// `ExtendImage(0.03)`: columns added on each side (they repeat the other end of the seam circle).
const EXTEND: usize = 6;
const XW: usize = W + 2 * EXTEND;
/// The rows of the MGP region: rows 40..60 of the seam sphere. Lens 0 has unknowns in rows 40..52, lens 1 in rows
/// 48..60. The overlap is rows 48..52.
const CROP0: usize = 40;
const CROP_ROWS: usize = 20;
const LENS_ROWS: [std::ops::Range<usize>; 2] = [0..12, 8..20];
const OVERLAP: std::ops::Range<usize> = 8..12;
const NCC_MIN: f64 = 0.96;
const SSD_MAX: f64 = 6400.0;
const LAMBDA: f64 = 0.1;
const CG_TOLERANCE: f64 = 1e-4;
/// The columns of the robust range: 6 + (int)(0.125 * H) from each end of the extended row.
const RANGE_COLUMNS: std::ops::Range<usize> = 18..194;

/// The gain map of each lens on the 200 x 100 camera-frame panorama: [r, g, b] for each pixel, row by row.
#[derive(Debug, Clone, PartialEq)]
pub struct FusionMaps {
    pub gain: [Vec<[f32; 3]>; 2],
}

impl FusionMaps {
    /// No colour change.
    pub fn identity() -> Self {
        FusionMaps {
            gain: [vec![[1.0; 3]; W * H], vec![[1.0; 3]; W * H]],
        }
    }

    /// Gives the gain of lens `l` for the direction `d` in the camera frame (bilinear).
    pub fn gain_at(&self, l: usize, d: &Vec3) -> [f32; 3] {
        let p = pano_from_camera(d);
        let theta = p[2].clamp(-1.0, 1.0).acos();
        let phi = p[1].atan2(p[0]).rem_euclid(std::f64::consts::TAU);
        let r = (theta * (H - 1) as f64 / std::f64::consts::PI).clamp(0.0, (H - 1) as f64);
        let c = phi * W as f64 / std::f64::consts::TAU;
        let (r0, c0) = (r.floor() as usize, c.floor() as usize);
        let (fr, fc) = ((r - r0 as f64) as f32, (c - c0 as f64) as f32);
        let r1 = (r0 + 1).min(H - 1);
        let at = |r: usize, c: usize| self.gain[l][r * W + c % W];
        std::array::from_fn(|k| {
            let top = at(r0, c0)[k] * (1.0 - fc) + at(r0, c0 + 1)[k] * fc;
            let bottom = at(r1, c0)[k] * (1.0 - fc) + at(r1, c0 + 1)[k] * fc;
            top * (1.0 - fr) + bottom * fr
        })
    }
}

/// The direction of seam-sphere position (row, column) in the panorama frame: the seam sphere turned by Ry(pi/2).
fn seam_direction(row: f64, col: f64) -> Vec3 {
    let (st, ct) = (row * std::f64::consts::PI / H as f64).sin_cos();
    let (sp, cp) = (col * std::f64::consts::TAU / W as f64).sin_cos();
    let v = [st * cp, st * sp, ct];
    [v[2], v[1], -v[0]]
}

/// Computes the gain maps for one image. `sample(lens, u, v)` gives the colour [r, g, b] (0 to 255) at the lens
/// pixel position (u, v), bilinear. `size` is the lens image size (pixels). The model must have its level
/// rotation; the module uses the camera frame.
pub fn fusion_maps(
    model: &LensModel,
    size: usize,
    sample: impl Fn(usize, f64, f64) -> [f32; 3],
) -> FusionMaps {
    let unlevel = transpose(&model.level);
    let lens_position = |d: &Vec3| {
        let ray = model.ray(&apply(&unlevel, &camera_from_pano(d)));
        [(ray.u[0], ray.v[0]), (ray.u[1], ray.v[1])]
    };
    // The invalid mask: a band position outside one of the two lens images.
    let inside = |(u, v): (f64, f64)| {
        (0.0..=1.0).contains(&(u / size as f64)) && (0.0..=1.0).contains(&(v / size as f64))
    };
    let mut invalid = vec![false; BAND.len() * W];
    for (i, &row) in BAND.iter().enumerate() {
        for col in 0..W {
            let p = lens_position(&seam_direction(row as f64, col as f64));
            invalid[i * W + col] = !(inside(p[0]) && inside(p[1]));
        }
    }
    // The band images: 800 x 16 bilinear samples (rounded to 8 bits), then the mean of each 4 x 4 block.
    let (bw, bh) = BAND_SAMPLES;
    let mut band = [
        vec![[0u8; 3]; BAND.len() * W],
        vec![[0u8; 3]; BAND.len() * W],
    ];
    let mut sums = [
        vec![[0u32; 3]; BAND.len() * W],
        vec![[0u32; 3]; BAND.len() * W],
    ];
    for y in 0..bh {
        let sy = ((y as f64 + 0.5) * BAND.len() as f64 / bh as f64 - 0.5)
            .clamp(0.0, (BAND.len() - 1) as f64);
        for x in 0..bw {
            let sx = ((x as f64 + 0.5) * W as f64 / bw as f64 - 0.5).clamp(0.0, (W - 1) as f64);
            let p = lens_position(&seam_direction(BAND[0] as f64 + sy, sx));
            let cell = (y * BAND.len() / bh) * W + x * W / bw;
            for l in 0..2 {
                let c = sample(l, p[l].0, p[l].1);
                for k in 0..3 {
                    sums[l][cell][k] += c[k].round().clamp(0.0, 255.0) as u32;
                }
            }
        }
    }
    let block = ((bw / W) * (bh / BAND.len())) as f32;
    for l in 0..2 {
        for (out, sum) in band[l].iter_mut().zip(&sums[l]) {
            *out = std::array::from_fn(|k| (sum[k] as f32 / block).round() as u8);
        }
    }
    mgp(&band, &invalid)
}

/// The MGP colour correction (`ImageFusionMGPCpu::GetRatioMapFast`) of the two band images [r, g, b] (4 x 200,
/// rows 48..52 of the seam sphere), and the panorama gain maps.
fn mgp(band: &[Vec<[u8; 3]>; 2], invalid: &[bool]) -> FusionMaps {
    // ExtendPixelValue (rows above and below repeat the band edge rows) and ExtendImage (wrapped columns), then
    // the crop of rows 40..60: crop row r is seam row 40 + r.
    let at = |l: usize, r: usize, x: usize| -> [f64; 3] {
        let row = (CROP0 + r).clamp(BAND[0], BAND[3]) - BAND[0];
        let col = (x + W - EXTEND) % W;
        band[l][row * W + col].map(f64::from)
    };
    let bad = |r: usize, x: usize| invalid[(r - OVERLAP.start) * W + (x + W - EXTEND) % W];

    // The NCC mask on the overlap, interior pixels: 3 x 3 x 3 samples.
    let (oh, mut ncc_pass, mut both_pass) = (
        OVERLAP.len(),
        vec![false; OVERLAP.len() * XW],
        vec![false; OVERLAP.len() * XW],
    );
    for i in 1..oh - 1 {
        for x in 1..XW - 1 {
            let mut a = Vec::with_capacity(27);
            let mut b = Vec::with_capacity(27);
            for di in 0..3 {
                for dx in 0..3 {
                    let r = OVERLAP.start + i + di - 1;
                    a.extend(at(0, r, x + dx - 1));
                    b.extend(at(1, r, x + dx - 1));
                }
            }
            let (ma, mb) = (a.iter().sum::<f64>() / 27.0, b.iter().sum::<f64>() / 27.0);
            let (mut sab, mut saa, mut sbb, mut ssd) = (0.0, 0.0, 0.0, 0.0);
            for (p, q) in a.iter().zip(&b) {
                sab += (p - ma) * (q - mb);
                saa += (p - ma) * (p - ma);
                sbb += (q - mb) * (q - mb);
                ssd += (p - q) * (p - q);
            }
            let ncc = sab / ((saa * sbb).sqrt() + 1e-8);
            ncc_pass[i * XW + x] = ncc > NCC_MIN;
            both_pass[i * XW + x] = ncc > NCC_MIN && ssd / 27.0 < SSD_MAX;
        }
    }
    let (n_ncc, n_both) = (
        ncc_pass.iter().filter(|&&v| v).count(),
        both_pass.iter().filter(|&&v| v).count(),
    );
    let mut mask = if n_ncc == 0 || n_both as f64 / n_ncc as f64 <= 0.5 {
        ncc_pass
    } else {
        both_pass
    };
    for i in 0..oh {
        for x in 0..XW {
            if bad(OVERLAP.start + i, x) {
                mask[i * XW + x] = false;
            }
        }
    }

    // The robust range of L - R per channel.
    let mut diffs: [Vec<f64>; 3] = Default::default();
    for i in 0..oh {
        for x in RANGE_COLUMNS {
            if mask[i * XW + x] {
                let (a, b) = (at(0, OVERLAP.start + i, x), at(1, OVERLAP.start + i, x));
                for k in 0..3 {
                    diffs[k].push(a[k] - b[k]);
                }
            }
        }
    }
    let n = diffs[0].len();
    if n == 0 {
        return FusionMaps::identity();
    }
    let (mut lo, mut hi, mut dmax) = ([0.0; 3], [0.0; 3], 0.0f64);
    for k in 0..3 {
        diffs[k].sort_by(f64::total_cmp);
        let cut = (0.2 * n as f64) as usize;
        let (p20, p80) = (diffs[k][cut], diffs[k][n - 1 - cut]);
        (lo[k], hi[k]) = (p20 - 10.0, p80 + 10.0);
        dmax = dmax.max(p20.abs()).max(p80.abs());
    }
    if lo.iter().sum::<f64>() + hi.iter().sum::<f64>() < 0.0 {
        hi = hi.map(|v| v.max(0.0));
    } else {
        lo = lo.map(|v| v.min(0.0));
    }
    let t = dmax.clamp(20.0, 100.0);
    let slope = 4.0 / t;
    let (lo2, hi2) = (lo.map(|v| v - t), hi.map(|v| v + t));
    let dmax_int = dmax as i64;
    let cg_iterations = if dmax_int <= 20 {
        5
    } else if dmax_int >= 121 {
        25
    } else {
        ((dmax_int - 20) / 5 + 5) as usize
    };

    // The unknowns: one for each pixel of each lens in its rows, in row-major order, lens 0 before lens 1.
    let mut id = [
        vec![usize::MAX; CROP_ROWS * XW],
        vec![usize::MAX; CROP_ROWS * XW],
    ];
    let mut count = 0;
    for r in 0..CROP_ROWS {
        for x in 0..XW {
            for l in 0..2 {
                if LENS_ROWS[l].contains(&r) {
                    id[l][r * XW + x] = count;
                    count += 1;
                }
            }
        }
    }
    // The data rows: w x_0 - w x_1 = YUV(1) - YUV(0) for each overlap pixel with m > 0.
    let mut data = Vec::new();
    for i in 0..oh {
        let r = OVERLAP.start + i;
        for x in 0..XW {
            if !mask[i * XW + x] {
                continue;
            }
            let (a, b) = (at(0, r, x), at(1, r, x));
            let d: [f64; 3] = std::array::from_fn(|k| a[k] - b[k]);
            let m = if (0..3).any(|k| d[k] < lo2[k] || d[k] > hi2[k]) {
                0.0
            } else if (0..3).all(|k| d[k] >= lo[k] && d[k] <= hi[k]) {
                1.0
            } else {
                (0..3)
                    .map(|k| (lo[k] - d[k]).max(d[k] - hi[k]).max(0.0))
                    .fold(0.0, f64::max)
            };
            if m > 0.0 {
                let w = (m - 1.0) * slope + 1.0;
                let (ya, yb) = (yuv(&a), yuv(&b));
                data.push((
                    id[0][r * XW + x],
                    id[1][r * XW + x],
                    w,
                    std::array::from_fn::<f64, 3, _>(|k| yb[k] - ya[k]),
                ));
            }
        }
    }
    // The smoothness rows: lambda (x_p - x_n) for each pixel and its right and lower neighbours of the same lens.
    let mut edges = Vec::new();
    for l in 0..2 {
        for r in LENS_ROWS[l].clone() {
            for x in 0..XW {
                let p = id[l][r * XW + x];
                if x + 1 < XW {
                    edges.push((p, id[l][r * XW + x + 1]));
                }
                if LENS_ROWS[l].contains(&(r + 1)) {
                    edges.push((p, id[l][(r + 1) * XW + x]));
                }
            }
        }
    }
    let mut diagonal = vec![0.0; count];
    for &(p, q, w, _) in &data {
        diagonal[p] += w * w;
        diagonal[q] += w * w;
    }
    for &(p, q) in &edges {
        diagonal[p] += LAMBDA * LAMBDA;
        diagonal[q] += LAMBDA * LAMBDA;
    }
    let apply_normal = |x: &[f64], out: &mut [f64]| {
        out.fill(0.0);
        for &(p, q, w, _) in &data {
            let r = w * (x[p] - x[q]);
            out[p] += w * r;
            out[q] -= w * r;
        }
        for &(p, q) in &edges {
            let r = LAMBDA * LAMBDA * (x[p] - x[q]);
            out[p] += r;
            out[q] -= r;
        }
    };
    let solution: [Vec<f64>; 3] = std::array::from_fn(|k| {
        let mut rhs = vec![0.0; count];
        for &(p, q, w, b) in &data {
            rhs[p] += w * b[k];
            rhs[q] -= w * b[k];
        }
        conjugate_gradient(&apply_normal, &diagonal, &rhs, cg_iterations)
    });

    // The corrected colours and the ratio maps on the seam sphere (rows 35..66).
    let mut ratio = [vec![[1.0f64; 3]; H * W], vec![[1.0f64; 3]; H * W]];
    for l in 0..2 {
        let mut extended = vec![[1.0f64; 3]; CROP_ROWS * XW];
        for r in LENS_ROWS[l].clone() {
            for x in 0..XW {
                let original = at(l, r, x);
                let [y, u, v] = yuv(&original);
                let j = id[l][r * XW + x];
                let (y, u, v) = (
                    y + solution[0][j],
                    u + solution[1][j] - 128.0,
                    v + solution[2][j] - 128.0,
                );
                // Rounded (OpenCV `saturate_cast`). With truncation, the seam of our test photos got darker than
                // in the Studio export.
                let corrected = [y + 1.402 * v, y - 0.34414 * u - 0.71414 * v, y + 1.772 * u]
                    .map(|c| c.round().clamp(0.0, 255.0));
                extended[r * XW + x] =
                    std::array::from_fn(|k| (255.0 + corrected[k]) / (255.0 + original[k]));
            }
        }
        // FuseLeftAndRightSide: fold the 212 extended columns back to 200; the repeated columns are averaged.
        for r in LENS_ROWS[l].clone() {
            for c in 0..W {
                let main = extended[r * XW + c + EXTEND];
                let other = if c < EXTEND {
                    Some(extended[r * XW + c + W + EXTEND])
                } else if c >= W - EXTEND {
                    Some(extended[r * XW + c + EXTEND - W])
                } else {
                    None
                };
                ratio[l][(CROP0 + r) * W + c] = match other {
                    Some(o) => std::array::from_fn(|k| (main[k] + o[k]) * 0.5),
                    None => main,
                };
            }
        }
    }
    // A 3 x 11 box blur on rows 35..55 (lens 0) and 45..65 (lens 1): columns wrap, rows reflect (101).
    let blur_rows = [35..55, 45..65];
    for l in 0..2 {
        let rows = blur_rows[l].clone();
        let source: Vec<[f64; 3]> = ratio[l][rows.start * W..rows.end * W].to_vec();
        let n = rows.len() as isize;
        let reflect = |i: isize| -> usize {
            let i = if i < 0 { -i } else { i };
            (if i >= n { 2 * (n - 1) - i } else { i }) as usize
        };
        for r in 0..rows.len() {
            for c in 0..W {
                let mut sum = [0.0; 3];
                for dr in -5..=5isize {
                    for dc in -1..=1isize {
                        let s = source[reflect(r as isize + dr) * W
                            + (c as isize + dc).rem_euclid(W as isize) as usize];
                        for k in 0..3 {
                            sum[k] += s[k];
                        }
                    }
                }
                ratio[l][(rows.start + r) * W + c] = sum.map(|v| v / 33.0);
            }
        }
    }
    // InverseRemap to the panorama: each panorama pixel samples the seam sphere (bilinear); outside the rows of a
    // lens (35..55, 45..65), the gain is 1.
    let valid_rows = [35.0..=55.0, 45.0..=65.0];
    let gain: [Vec<[f32; 3]>; 2] = std::array::from_fn(|l| {
        let mut out = vec![[1.0f32; 3]; W * H];
        for pr in 0..H {
            for pc in 0..W {
                let (st, ct) = (pr as f64 * std::f64::consts::PI / (H - 1) as f64).sin_cos();
                let (sp, cp) = (pc as f64 * std::f64::consts::TAU / W as f64).sin_cos();
                let u = [st * cp, st * sp, ct];
                // Ry(-pi/2): back to the seam sphere.
                let v = [-u[2], u[1], u[0]];
                let row = v[2].clamp(-1.0, 1.0).acos() * H as f64 / std::f64::consts::PI;
                if !valid_rows[l].contains(&row) {
                    continue;
                }
                let col = v[1].atan2(v[0]).rem_euclid(std::f64::consts::TAU) * W as f64
                    / std::f64::consts::TAU;
                let (r0, c0) = (row.floor() as usize, col.floor() as usize);
                let (fr, fc) = (row - r0 as f64, col - c0 as f64);
                let get = |r: usize, c: usize| ratio[l][r.min(H - 1) * W + c % W];
                let bgr: [f64; 3] = std::array::from_fn(|k| {
                    (get(r0, c0)[k] * (1.0 - fc) + get(r0, c0 + 1)[k] * fc) * (1.0 - fr)
                        + (get(r0 + 1, c0)[k] * (1.0 - fc) + get(r0 + 1, c0 + 1)[k] * fc) * fr
                });
                out[pr * W + pc] = bgr.map(|v| v as f32);
            }
        }
        out
    });
    FusionMaps { gain }
}

/// Full-range BT.601 (JPEG) Y, U, V of an [r, g, b] colour.
fn yuv(c: &[f64; 3]) -> [f64; 3] {
    let [r, g, b] = *c;
    [
        0.299 * r + 0.587 * g + 0.114 * b,
        -0.168736 * r - 0.331264 * g + 0.5 * b + 128.0,
        0.5 * r - 0.418688 * g - 0.081312 * b + 128.0,
    ]
}

/// Eigen's `ConjugateGradient` with a diagonal preconditioner, from x = 0: at most `iterations` steps, or until
/// the residual is less than `CG_TOLERANCE` times the right side.
fn conjugate_gradient(
    apply_matrix: &impl Fn(&[f64], &mut [f64]),
    diagonal: &[f64],
    rhs: &[f64],
    iterations: usize,
) -> Vec<f64> {
    let n = rhs.len();
    let mut x = vec![0.0; n];
    let dot = |a: &[f64], b: &[f64]| a.iter().zip(b).map(|(p, q)| p * q).sum::<f64>();
    let rhs_norm2 = dot(rhs, rhs);
    if rhs_norm2 == 0.0 {
        return x;
    }
    let threshold = (CG_TOLERANCE * CG_TOLERANCE * rhs_norm2).max(f64::MIN_POSITIVE);
    let inverse: Vec<f64> = diagonal
        .iter()
        .map(|&d| if d != 0.0 { 1.0 / d } else { 1.0 })
        .collect();
    let mut residual = rhs.to_vec();
    if dot(&residual, &residual) < threshold {
        return x;
    }
    let mut p: Vec<f64> = residual.iter().zip(&inverse).map(|(r, i)| r * i).collect();
    let mut abs_new = dot(&residual, &p);
    let mut tmp = vec![0.0; n];
    for _ in 0..iterations {
        apply_matrix(&p, &mut tmp);
        let alpha = abs_new / dot(&p, &tmp);
        for i in 0..n {
            x[i] += alpha * p[i];
            residual[i] -= alpha * tmp[i];
        }
        if dot(&residual, &residual) < threshold {
            break;
        }
        let z: Vec<f64> = residual.iter().zip(&inverse).map(|(r, i)| r * i).collect();
        let abs_old = abs_new;
        abs_new = dot(&residual, &z);
        let beta = abs_new / abs_old;
        for i in 0..n {
            p[i] = z[i] + beta * p[i];
        }
    }
    x
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A textured band: the same scene in both lenses, lens 1 with a colour change.
    fn bands(change: impl Fn([u8; 3]) -> [u8; 3]) -> [Vec<[u8; 3]>; 2] {
        let scene: Vec<[u8; 3]> = (0..BAND.len() * W)
            .map(|i| {
                let (r, c) = (i / W, i % W);
                let v = 100.0
                    + 60.0 * ((c as f64 * 0.7).sin() + (r as f64 * 1.3 + c as f64 * 0.2).cos());
                [v as u8, (v * 0.8) as u8, (v * 0.6) as u8]
            })
            .collect();
        let other = scene.iter().map(|&p| change(p)).collect();
        [scene, other]
    }

    #[test]
    fn the_same_colours_need_no_correction() {
        let maps = mgp(&bands(|p| p), &vec![false; BAND.len() * W]);
        for l in 0..2 {
            assert!(maps.gain[l]
                .iter()
                .flatten()
                .all(|g| (g - 1.0).abs() < 1e-6));
        }
    }

    #[test]
    fn a_darker_lens_gets_lighter_and_the_other_darker_at_the_seam() {
        let maps = mgp(
            &bands(|p| p.map(|v| v.saturating_sub(12))),
            &vec![false; BAND.len() * W],
        );
        // At the seam (panorama column 50 is on the seam at the equator row 49 of lens 0 / 1).
        let seam = |l: usize| maps.gain[l][49 * W + 50];
        assert!(seam(0).iter().all(|&g| g < 1.0), "{:?}", seam(0));
        assert!(seam(1).iter().all(|&g| g > 1.0), "{:?}", seam(1));
        // Far from the seam, no change.
        assert_eq!(maps.gain[0][49 * W], [1.0; 3]);
    }

    #[test]
    fn invalid_band_positions_give_no_correction() {
        let maps = mgp(
            &bands(|p| p.map(|v| v.saturating_sub(12))),
            &vec![true; BAND.len() * W],
        );
        assert_eq!(maps, FusionMaps::identity());
    }

    #[test]
    fn the_conjugate_gradient_solves_a_small_system() {
        // [[4, 1], [1, 3]] x = [1, 2] -> x = [1/11, 7/11].
        let m = |x: &[f64], out: &mut [f64]| {
            out[0] = 4.0 * x[0] + x[1];
            out[1] = x[0] + 3.0 * x[1];
        };
        let x = conjugate_gradient(&m, &[4.0, 3.0], &[1.0, 2.0], 10);
        assert!(
            (x[0] - 1.0 / 11.0).abs() < 1e-9 && (x[1] - 7.0 / 11.0).abs() < 1e-9,
            "{x:?}"
        );
    }
}
