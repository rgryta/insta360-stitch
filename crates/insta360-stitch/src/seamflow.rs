//! This module makes the seam flow of the Insta360 Studio photo export (`SeamlessBlender`, DIS mode 5). Studio finds
//! the parallax between the two lenses at the seam with an optical flow. Then it moves the sample positions of each
//! lens in the band of 82 to 98 degrees from the axis of lens 0, so that the two images agree at the seam.
//!
//! - Two grey strips of 60 x 1080 pixels (one for each lens) show the same directions along the seam circle. The
//!   rows go from -200 to +200 degrees on the circle (0 is the zenith, +-180 is the nadir). The columns go across the
//!   seam, and column 0 is on the lens 0 side.
//! - A mask gives the strip pixels that both lenses see (`calcLineImageMask`).
//! - The DIS flow with the FDS changes (see `dis`) goes from lens 0 to lens 1 (L2R) and from lens 1 to lens 0 (R2L),
//!   three runs each.
//! - Near the nadir, the flow of a row and the flow of its duplicate row (360 degrees away) are cross-faded
//!   (`blendFlow`).
//! - On the 200 x 100 map of each lens, a map pixel in the band moves by (1 - a) of the flow, with `a` the flow weight
//!   of that lens. Thus, the two lenses meet at the centre of the band. Studio renders from the 200 x 100 maps. Here,
//!   the change of each map pixel is interpolated and added to the exact position of each output pixel.

use crate::blend::{
    bilinear, camera_from_pano, grid_direction, grid_position, pano_from_camera, H as GRID_H,
    W as GRID_W,
};
use crate::dis::{dis, gaussian_blur, Fds, Params, Plane};
use crate::lens::LensModel;
use crate::math::*;
use std::f64::consts::TAU;

const COLS: usize = 60;
const ROWS: usize = 1080;
/// The angle of one strip row (degrees). The strip has 400 degrees on 1080 rows.
const DELTA_DEG: f64 = 400.0 / 1079.0;
/// The flow weight of lens 0 is 1 to `BAND.0` degrees from the axis of lens 0 and 0 from `BAND.1` degrees.
const BAND: (f64, f64) = (82.0, 98.0);
/// The flow limits (x, y) in strip pixels: L2R and R2L (`FDSFlow` limits of camera type 0x16).
const L2R_LIMITS: [(f32, f32); 2] = [(-60.0, 4.0), (-2.0, 2.0)];
const R2L_LIMITS: [(f32, f32); 2] = [(-4.0, 60.0), (-2.0, 2.0)];
/// `erodeBeltMasksUsingFovTable`: the usable field of view (degrees from the lens axis) as a function of the angle
/// from straight down in the lens image (degrees), for each lens of the X5.
const BELT: [&[(f64, f64)]; 2] = [
    &[(0.0, 92.5), (11.0, 92.5), (47.0, 100.0)],
    &[
        (0.0, 92.5),
        (10.0, 92.5),
        (18.0, 94.5),
        (24.0, 96.0),
        (28.0, 97.5),
        (42.0, 100.0),
    ],
];
/// The strip rows that the belt tables erode: the parts near the nadir (fractions of the rows).
const BELT_ROWS: [(f64, f64); 2] = [(0.0, 0.2), (0.8, 1.0)];
/// With the lens guard, Studio keeps the columns of 13 degrees across the seam in the mask of lens 0.
const GUARD_KEEP_DEG: f64 = 13.0;

/// The strip pixels per radian across the seam (the same scale as along the seam).
fn radius_px() -> f64 {
    360.0 / DELTA_DEG / TAU
}

/// The camera-frame direction of strip position (u, r).
fn strip_direction(u: f64, r: f64) -> Vec3 {
    let theta = (-200.0 + r * DELTA_DEG).to_radians();
    let t = ((COLS - 1) as f64 / 2.0 - u) / radius_px();
    camera_from_pano(&normalize(&[t, -theta.sin(), theta.cos()]))
}

/// The strip position (u, r) of a camera-frame direction (`sph2strip`). The angle on the circle is in -180..180
/// degrees.
fn strip_position(d: &Vec3) -> (f64, f64) {
    let p = pano_from_camera(d);
    let n = p[1].hypot(p[2]);
    let mut theta = (-p[1]).atan2(p[2]).to_degrees();
    if theta >= 180.0 {
        theta -= 360.0;
    }
    (
        (COLS - 1) as f64 / 2.0 - radius_px() * p[0] / n,
        (theta + 200.0) / DELTA_DEG,
    )
}

/// The flow weight of lens 0 for a camera-frame direction.
fn flow_weight0(d: &Vec3) -> f64 {
    let phi0 = (-d[2] / norm(d)).clamp(-1.0, 1.0).acos().to_degrees();
    (1.0 - (phi0 - BAND.0) / (BAND.1 - BAND.0)).clamp(0.0, 1.0)
}

/// Bilinear sample of a strip plane at (u, r), clamped to the strip.
fn strip_bilinear(f: &Plane<f32>, u: f64, r: f64) -> f64 {
    f64::from(bilinear(&f.data, COLS, ROWS, u, r, false))
}

/// The lens position of each strip pixel for one lens, and if the lens sees it.
struct StripMap {
    pos: Vec<(f64, f64)>,
    visible: Vec<bool>,
}

/// The strip maps of the two lenses.
fn strip_maps(model: &LensModel) -> [StripMap; 2] {
    let unlevel = transpose(&model.level);
    let samples: Vec<_> = (0..COLS * ROWS)
        .map(|k| {
            let d = strip_direction((k % COLS) as f64, (k / COLS) as f64);
            model.ray(&apply(&unlevel, &d))
        })
        .collect();
    std::array::from_fn(|l| StripMap {
        pos: samples.iter().map(|s| (s.u[l], s.v[l])).collect(),
        visible: samples.iter().map(|s| s.phi[l] <= model.half_fov).collect(),
    })
}

/// The change of the sample position in each lens, on the 200 x 100 map of the camera-frame panorama.
#[derive(Debug, Clone)]
pub struct SeamFlow {
    du: [Vec<f32>; 2],
    dv: [Vec<f32>; 2],
}

impl SeamFlow {
    /// Makes the seam flow of a photo with lens images of `size` pixels. `grey(lens, u, v)` gives the grey value
    /// (0-255) at lens pixel (u, v). `None` for a model without the Studio blend.
    pub fn new(
        model: &LensModel,
        size: f64,
        grey: impl Fn(usize, f64, f64) -> f32,
    ) -> Option<Self> {
        let guard = model.blend.as_ref()?.guard;
        let unlevel = transpose(&model.level);
        let ray = |d: &Vec3| model.ray(&apply(&unlevel, d));
        let maps = strip_maps(model);
        // The strips: Studio renders 180 x 3240 and reduces with INTER_AREA, then blurs (5 x 5, sigma 0.8).
        // [inf] The order of the grey conversion and the reduction.
        let mut sums = [vec![0f32; COLS * ROWS], vec![0f32; COLS * ROWS]];
        for k in 0..COLS * ROWS {
            let (u, r) = ((k % COLS) as f64, (k / COLS) as f64);
            for i in 0..3 {
                for j in 0..3 {
                    let s = ray(&strip_direction(
                        u + (i as f64 + 0.5) / 3.0 - 0.5,
                        r + (j as f64 + 0.5) / 3.0 - 0.5,
                    ));
                    for (l, sum) in sums.iter_mut().enumerate() {
                        sum[k] += grey(l, s.u[l], s.v[l]);
                    }
                }
            }
        }
        let strips: [Plane<u8>; 2] = std::array::from_fn(|l| {
            let mean = Plane {
                w: COLS,
                h: ROWS,
                data: sums[l].iter().map(|v| (v / 9.0).round()).collect(),
            };
            Plane {
                w: COLS,
                h: ROWS,
                data: gaussian_blur(&mean, 5, 0.8)
                    .data
                    .iter()
                    .map(|v| v.round().clamp(0.0, 255.0) as u8)
                    .collect(),
            }
        });
        let mask = line_mask(model, &maps, size, guard);
        let params = Params {
            finest_scale: 1,
            max_coarsest_scale: 2,
            patch_size: 8,
            patch_stride: 3,
            grad_descent_iter: 12,
            variational_refinement_iter: 5,
            stripes: 1,
            ..Params::default()
        };
        let run = |i0: &Plane<u8>, i1: &Plane<u8>, limits| {
            let fds = Fds {
                mask0: mask.clone(),
                mask1: mask.clone(),
                limits,
            };
            let mut f = dis(&params, i0, i1, None, Some(&fds));
            for _ in 0..2 {
                f = dis(&params, i0, i1, Some(&f), Some(&fds));
            }
            f
        };
        let mut l2r = run(&strips[0], &strips[1], L2R_LIMITS);
        let mut r2l = run(&strips[1], &strips[0], R2L_LIMITS);
        for f in [&mut l2r, &mut r2l] {
            nadir_fade(&mut f.0);
            nadir_fade(&mut f.1);
        }
        Some(Self::update(model, &maps, [&r2l, &l2r], size))
    }

    /// `updateMap`: the change of the position of each map pixel in the band. Lens 0 uses R2L, lens 1 uses L2R.
    fn update(
        model: &LensModel,
        maps: &[StripMap; 2],
        flows: [&(Plane<f32>, Plane<f32>); 2],
        size: f64,
    ) -> Self {
        let unlevel = transpose(&model.level);
        let mut du: [Vec<f32>; 2] = std::array::from_fn(|_| vec![0.0; GRID_W * GRID_H]);
        let mut dv = du.clone();
        let (umax, rmax) = ((COLS - 1) as f64, (ROWS - 1) as f64);
        for k in 0..GRID_W * GRID_H {
            let d = camera_from_pano(&grid_direction(k / GRID_W, k % GRID_W));
            let a0 = flow_weight0(&d);
            if !(0.0 < a0 && a0 < 1.0) {
                continue;
            }
            let exact = model.ray(&apply(&unlevel, &d));
            let (u, r) = strip_position(&d);
            let (u, r) = (u.clamp(0.0, umax), r.clamp(0.0, rmax));
            for l in 0..2 {
                let a = if l == 0 { a0 } else { 1.0 - a0 };
                let (fx, fy) = flows[l];
                let q = (
                    (u + (1.0 - a) * strip_bilinear(fx, u, r)).clamp(0.0, umax),
                    (r + (1.0 - a) * strip_bilinear(fy, u, r)).clamp(0.0, rmax),
                );
                // Studio keeps the map pixel if a strip neighbour of q, or the new position, is not valid.
                let (u0, r0) = (q.0.floor() as usize, q.1.floor() as usize);
                let (u1, r1) = ((u0 + 1).min(COLS - 1), (r0 + 1).min(ROWS - 1));
                let map = &maps[l];
                if [(u0, r0), (u1, r0), (u0, r1), (u1, r1)]
                    .iter()
                    .any(|&(x, y)| !map.visible[y * COLS + x])
                {
                    continue;
                }
                let (fu, fr) = (q.0 - u0 as f64, q.1 - r0 as f64);
                let at = |x: usize, y: usize| map.pos[y * COLS + x];
                let lerp = |a: (f64, f64), b: (f64, f64), t: f64| {
                    (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t)
                };
                let m = lerp(
                    lerp(at(u0, r0), at(u1, r0), fu),
                    lerp(at(u0, r1), at(u1, r1), fu),
                    fr,
                );
                if (m.0 + l as f64 * size) / (2.0 * size) <= 1e-8 || m.1 / size <= 1e-8 {
                    continue;
                }
                du[l][k] = (m.0 - exact.u[l]) as f32;
                dv[l][k] = (m.1 - exact.v[l]) as f32;
            }
        }
        SeamFlow { du, dv }
    }

    /// The change of the sample position (pixels) in each lens for a camera-frame unit direction `d`. It is 0 far
    /// from the band.
    pub fn shift(&self, d: &Vec3) -> Option<[(f32, f32); 2]> {
        // A map cell is less than 3 degrees wide; thus, the map is 0 at 12 degrees from the seam circle.
        if d[2].abs() > 12f64.to_radians().sin() {
            return None;
        }
        let (r, c) = grid_position(d);
        Some(std::array::from_fn(|l| {
            (
                bilinear(&self.du[l], GRID_W, GRID_H, c, r, true),
                bilinear(&self.dv[l], GRID_W, GRID_H, c, r, true),
            )
        }))
    }
}

/// `blendFlow(2.0)`: for the rows at -181 to -179 degrees, the row and its duplicate (360 degrees away) both get
/// (1 - w) of the duplicate plus w of the row, with w = |theta + 181| / 2. [inf] The duplicate is the nearest row.
fn nadir_fade(f: &mut Plane<f32>) {
    let period = 360.0 / DELTA_DEG;
    for r in 0..ROWS {
        let theta = -200.0 + r as f64 * DELTA_DEG;
        if !(-181.0..=-179.0).contains(&theta) {
            continue;
        }
        let dup = (r as f64 + period).round() as usize;
        if dup >= ROWS {
            continue;
        }
        let w = ((theta + 181.0).abs() / 2.0) as f32;
        for u in 0..COLS {
            let (a, b) = (f.data[r * COLS + u], f.data[dup * COLS + u]);
            let v = (1.0 - w) * b + w * a;
            f.data[r * COLS + u] = v;
            f.data[dup * COLS + u] = v;
        }
    }
}

/// `calcLineImageMask`: the strip pixels (255) that both lenses can use.
fn line_mask(model: &LensModel, maps: &[StripMap; 2], size: f64, guard: bool) -> Plane<u8> {
    let masks: [Vec<u8>; 2] = std::array::from_fn(|l| {
        let map = &maps[l];
        // A pixel is valid if its position is in the image: (0, 1] of the double-lens width and of the height.
        let mut m: Vec<u8> = (0..COLS * ROWS)
            .map(|k| {
                let (u, v) = map.pos[k];
                let (un, vn) = ((u + l as f64 * size) / (2.0 * size), v / size);
                let inside = un > 0.0 && un <= 1.0 && vn > 0.0 && vn <= 1.0;
                if inside && map.visible[k] {
                    255
                } else {
                    0
                }
            })
            .collect();
        // Studio erodes the mask (9 x 9) for lens type 113, but not for the lens-guard design (115).
        if !guard {
            m = erode(&m, 4);
        }
        m
    });
    let [mut m0, mut m1] = masks;
    let margin = if guard {
        let keep = (GUARD_KEEP_DEG / 400.0 * ROWS as f64) as usize;
        COLS.saturating_sub(keep) >> 1
    } else {
        0
    };
    for r in 0..ROWS {
        for u in (0..margin).chain(COLS - margin..COLS) {
            m0[r * COLS + u] = 0;
        }
    }
    for (l, m) in [(0, &mut m0), (1, &mut m1)] {
        erode_belt(model, &maps[l], l, size, guard, margin, m);
    }
    Plane {
        w: COLS,
        h: ROWS,
        data: m0.iter().zip(&m1).map(|(a, b)| a & b).collect(),
    }
}

/// OpenCV `erode` with a square of 2 `r` + 1 pixels; the border does not erode.
fn erode(m: &[u8], r: usize) -> Vec<u8> {
    let mut out = vec![0u8; m.len()];
    for y in 0..ROWS {
        for x in 0..COLS {
            let (y0, y1) = (y.saturating_sub(r), (y + r).min(ROWS - 1));
            let (x0, x1) = (x.saturating_sub(r), (x + r).min(COLS - 1));
            out[y * COLS + x] = (y0..=y1)
                .flat_map(|yy| (x0..=x1).map(move |xx| (xx, yy)))
                .map(|(xx, yy)| m[yy * COLS + xx])
                .min()
                .unwrap_or(0);
        }
    }
    out
}

/// `erodeBeltMasksUsingFovTable`: in the rows near the nadir, a pixel outside the usable field of view of the belt
/// table becomes 0. The scan of a row goes from the other lens's side to the lens's own side: lens 0 from column
/// 59 - margin down, lens 1 from column `margin` up. A pixel inside the field of view stops the scan if it is
/// valid; if not, it becomes valid.
fn erode_belt(
    model: &LensModel,
    map: &StripMap,
    l: usize,
    size: f64,
    guard: bool,
    margin: usize,
    m: &mut [u8],
) {
    let lens = &model.lenses[l];
    // The radius (lens pixels / size) at `fov` degrees from the axis, `a` degrees from straight down.
    let radius = |fov: f64, a: f64| {
        let (sf, cf) = fov.to_radians().sin_cos();
        let (sa, ca) = a.to_radians().sin_cos();
        let (du, dv) = model.offset(l, &[sf * sa, sf * ca, cf]);
        du.hypot(dv) / size
    };
    // Studio stops the table where it crosses half the lens field of view.
    let half_fov = if guard { 195.0 / 2.0 } else { 200.0 / 2.0 };
    let mut points: Vec<(f64, f64)> = Vec::new();
    for (k, &(a, f)) in BELT[l].iter().enumerate() {
        if f > half_fov {
            if let Some(&(pa, pf)) = BELT[l].get(k.wrapping_sub(1)) {
                let cross = pa + (half_fov - pf) / (f - pf) * (a - pa);
                points.push((cross, radius(half_fov, cross)));
            }
            break;
        }
        points.push((a, radius(f, a)));
    }
    let limit = |angle: f64| {
        for w in points.windows(2) {
            let ((a0, r0), (a1, r1)) = (w[0], w[1]);
            if a0 <= angle && angle < a1 {
                return r0 + (angle - a0) * (r1 - r0) / (a1 - a0);
            }
        }
        1.0
    };
    let columns: Vec<usize> = if l == 0 {
        (0..COLS - margin).rev().collect()
    } else {
        (margin..COLS).collect()
    };
    for &(f0, f1) in &BELT_ROWS {
        for r in (f0 * ROWS as f64) as usize..(f1 * ROWS as f64) as usize {
            for &u in &columns {
                let k = r * COLS + u;
                let (x, y) = map.pos[k];
                let (du, dv) = ((x - lens.cx) / size, (y - lens.cy) / size);
                let angle = (du / dv).abs().atan().to_degrees();
                let rr = limit(angle);
                if du * du + dv * dv > rr * rr {
                    m[k] = 0;
                } else if m[k] != 0 {
                    break;
                } else {
                    m[k] = 255;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_position_inverts_strip_direction() {
        for &(u, r) in &[(0.0, 60.0), (29.5, 540.0), (59.0, 1000.0), (12.3, 300.7)] {
            let (pu, pr) = strip_position(&strip_direction(u, r));
            assert!(
                (pu - u).abs() < 1e-9 && (pr - r).abs() < 1e-9,
                "{u} {r} -> {pu} {pr}"
            );
        }
        // Column 0 is on the lens 0 side: nearer to the axis of lens 0 (camera -z) than column 59.
        assert!(
            flow_weight0(&strip_direction(0.0, 540.0))
                > flow_weight0(&strip_direction(59.0, 540.0))
        );
    }

    #[test]
    fn the_nadir_fade_makes_the_row_and_its_duplicate_equal() {
        let mut f = Plane::new(COLS, ROWS);
        for (k, v) in f.data.iter_mut().enumerate() {
            *v = (k / COLS) as f32;
        }
        nadir_fade(&mut f);
        let period = 360.0 / DELTA_DEG;
        let rows: Vec<usize> = (0..ROWS)
            .filter(|&r| (-181.0..=-179.0).contains(&(-200.0 + r as f64 * DELTA_DEG)))
            .collect();
        assert!(!rows.is_empty());
        for r in rows {
            let dup = (r as f64 + period).round() as usize;
            let w = ((-200.0 + r as f64 * DELTA_DEG + 181.0).abs() / 2.0) as f32;
            let want = (1.0 - w) * dup as f32 + w * r as f32;
            assert!((f.data[r * COLS] - want).abs() < 1e-3, "row {r}");
            assert_eq!(f.data[r * COLS], f.data[dup * COLS]);
        }
    }

    /// A model like the X5 without a lens guard: lens 0 looks along camera -z, lens 1 along +z.
    fn x5_like(size: f64) -> LensModel {
        let lens = crate::lens::Lens {
            rotation: axis_rotation('y', 180.0),
            cx: size / 2.0,
            cy: size / 2.0,
            radius: size * 0.492,
            mei: None,
        };
        LensModel {
            lenses: [
                lens,
                crate::lens::Lens {
                    rotation: IDENTITY,
                    ..lens
                },
            ],
            coefficients: [0.0, 0.03159, -8.415e-5, 2.201e-6, -1.284e-8],
            half_fov: 100.0,
            level: IDENTITY,
            blend: None,
        }
    }

    #[test]
    fn the_belt_tables_erode_the_far_side_of_each_lens_near_the_nadir() {
        let size = 1000.0;
        let model = x5_like(size);
        let maps = strip_maps(&model);
        for (l, map) in maps.iter().enumerate() {
            let mut m = vec![255u8; COLS * ROWS];
            erode_belt(&model, map, l, size, false, 0, &mut m);
            // Row 0 is 20 degrees past the nadir. The far side of lens 0 is column 59, of lens 1 column 0.
            let (own, far) = if l == 0 { (0, COLS - 1) } else { (COLS - 1, 0) };
            assert_eq!((m[own], m[far]), (255, 0), "lens {l}");
            // Rows far from the nadir do not change.
            assert!(m[540 * COLS..541 * COLS].iter().all(|&v| v == 255));
        }
    }
}
