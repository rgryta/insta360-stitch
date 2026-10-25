//! This module makes the seam blend of the Insta360 Studio export (dynamic stitch type 2, blend angle 6 degrees,
//! fisheye mask and dynamic alpha on; `SeamlessBlenderImpl::getLeftTemplateAlpha`, `RefineAlphaWithFishmask`,
//! `TemplateBlenderImpl::calcFisheyeMaskONEX5AndProtector`).
//!
//! The weight of lens 0 has two parts:
//! - An alpha map of 200 x 100 on the camera-frame panorama: a linear ramp of 6 degrees at 90 degrees from the axis
//!   of lens 0. Below 65 degrees south, the ramp becomes wider, up to 15 degrees at 80 degrees south. The map is
//!   multiplied with the lens masks and cleaned near the nadir. If the ramp meets a masked part of a lens, the
//!   frame uses a ramp of 50 degrees.
//! - A mask of 800 x 800 for each lens image: 1 inside the usable part of the lens, 0 outside, with a soft edge of
//!   4.1 mask pixels. At the bottom of each lens image, the usable part ends nearer to the lens axis (from 90.5
//!   degrees straight down to 97 degrees at the side), because the camera body is there.
//!
//! For each output pixel, the weight w of the alpha map is changed to w m0 / (w m0 + (1 - w) m1), with m0 and m1 the
//! lens masks at the sample positions (as the Studio shader).

use crate::math::*;

pub(crate) const W: usize = 200;
pub(crate) const H: usize = 100;
const MASK: usize = 800;
/// `SeamlessBlenderImpl`: the blend angle of the export (6 degrees), the dynamic-alpha angle of the X5 (15 degrees)
/// and the angle of the second template (50 degrees).
const BLEND_DEG: f64 = 6.0;
const DYNAMIC_DEG: f64 = 15.0;
const FALLBACK_DEG: f64 = 50.0;
/// The mask feather of AI flow version 4: 1 / 4.1 per mask pixel.
const FEATHER: f32 = 0.243_902_44;

/// The usable field of view of a lens (degrees from its axis) as a function of the angle from straight down in the
/// lens image (degrees). Lens type 113: X5. Lens type 115: X5 with the premium lens guard.
type Table = &'static [(f64, f64)];
const X5: [Table; 2] = [
    &[
        (0.0, 90.5),
        (11.0, 90.5),
        (17.0, 92.0),
        (33.0, 95.7),
        (57.0, 97.0),
    ],
    &[
        (0.0, 91.5),
        (10.0, 91.5),
        (15.0, 92.0),
        (20.0, 93.0),
        (27.0, 94.0),
        (40.0, 97.0),
    ],
];
const X5_GUARD: [Table; 2] = [
    &[(0.0, 90.5), (11.0, 90.5), (17.0, 92.0), (33.0, 94.0)],
    &[
        (0.0, 91.5),
        (10.0, 91.5),
        (15.0, 92.0),
        (18.0, 93.5),
        (23.0, 94.0),
    ],
];

/// The seam blend of one lens model.
#[derive(Debug, Clone)]
pub struct Blend {
    alpha: Vec<f32>,
    mask: [Vec<f32>; 2],
    /// The size of the lens images (pixels).
    size: f64,
    /// The lens-guard design (lens type 115).
    pub(crate) guard: bool,
}

/// The panorama frame (x = lens 0 axis, z = zenith) from the camera frame of the lens model (lens 0 axis -z, up +y).
pub(crate) fn pano_from_camera(d: &Vec3) -> Vec3 {
    [-d[2], -d[0], d[1]]
}

pub(crate) fn camera_from_pano(p: &Vec3) -> Vec3 {
    [-p[1], p[2], -p[0]]
}

/// The direction of alpha-map pixel (row, column) in the panorama frame. The rows are on the poles.
pub(crate) fn grid_direction(r: usize, c: usize) -> Vec3 {
    let (st, ct) = (r as f64 * std::f64::consts::PI / (H - 1) as f64).sin_cos();
    let (sp, cp) = (std::f64::consts::TAU * (1.0 - c as f64 / W as f64)).sin_cos();
    [st * cp, st * sp, ct]
}

/// The alpha-map position (row, column) of a camera-frame direction.
pub(crate) fn grid_position(d: &Vec3) -> (f64, f64) {
    let p = pano_from_camera(d);
    let theta = (p[2] / norm(&p)).clamp(-1.0, 1.0).acos();
    let phi = p[1].atan2(p[0]).rem_euclid(std::f64::consts::TAU);
    (
        theta * (H - 1) as f64 / std::f64::consts::PI,
        (W as f64 * (1.0 - phi / std::f64::consts::TAU)).rem_euclid(W as f64),
    )
}

/// The weight of lens 0 of the template with a ramp half width of `half` degrees, for a panorama direction.
/// `widen` makes the ramp wider near the nadir (dynamic alpha).
pub fn template_weight(p: &Vec3, half: f64, widen: bool) -> f64 {
    let theta = (p[0] / norm(p)).clamp(-1.0, 1.0).acos().to_degrees();
    let hw = if widen {
        let (z, s65, s80) = (
            p[2] / norm(p),
            -(65f64.to_radians().sin()),
            -(80f64.to_radians().sin()),
        );
        let wide = half.max(DYNAMIC_DEG / 2.0);
        if z >= s65 {
            half
        } else if z <= s80 {
            wide
        } else {
            half + (wide - half) * (s65 - z) / (s65 - s80)
        }
    } else {
        half
    };
    if theta <= 90.0 - hw {
        1.0
    } else if theta <= 90.0 + hw {
        1.0 - (theta - (90.0 - hw)) / (2.0 * hw)
    } else {
        0.0
    }
}

/// The weight of lens 0 for a camera-frame direction without the masks (a model without a `Blend`).
pub fn template_weight_camera(d: &Vec3) -> f64 {
    template_weight(&pano_from_camera(d), BLEND_DEG / 2.0, true)
}

/// Bilinear sample of a `size` x `size` map at (x, y) (texel positions), clamped at the borders.
pub(crate) fn bilinear(
    map: &[f32],
    size_x: usize,
    size_y: usize,
    x: f64,
    y: f64,
    wrap_x: bool,
) -> f32 {
    let x = if wrap_x {
        x.rem_euclid(size_x as f64)
    } else {
        x.clamp(0.0, (size_x - 1) as f64)
    };
    let y = y.clamp(0.0, (size_y - 1) as f64);
    let (x0, y0) = (x.floor() as usize, y.floor() as usize);
    let (fx, fy) = ((x - x0 as f64) as f32, (y - y0 as f64) as f32);
    let x1 = if wrap_x {
        (x0 + 1) % size_x
    } else {
        (x0 + 1).min(size_x - 1)
    };
    let y1 = (y0 + 1).min(size_y - 1);
    let at = |x: usize, y: usize| map[y * size_x + x];
    (at(x0, y0) * (1.0 - fx) + at(x1, y0) * fx) * (1.0 - fy)
        + (at(x0, y1) * (1.0 - fx) + at(x1, y1) * fx) * fy
}

impl Blend {
    /// Makes the blend. `project(lens, lens_ray)` gives the position (pixels, from the lens centre) of a ray in the
    /// frame of that lens (axis +z, image down +y). `position(camera_direction)` gives the position of a
    /// camera-frame direction in each lens image (pixels). `centre` is the centre of each lens (pixels). `guard`
    /// selects the tables of the lens-guard design.
    pub fn new(
        size: f64,
        centre: [(f64, f64); 2],
        guard: bool,
        project: impl Fn(usize, &Vec3) -> (f64, f64),
        position: impl Fn(&Vec3) -> [(f64, f64); 2],
    ) -> Self {
        let (tables, max_fov) = if guard {
            (X5_GUARD, 195.0 / 2.0 - 2.0)
        } else {
            (X5, 200.0 / 2.0 - 2.0)
        };
        let mask = std::array::from_fn(|l| {
            lens_mask(tables[l], tables[0], max_fov, size, centre[l], |ray| {
                project(l, ray)
            })
        });
        let positions: Vec<[(f64, f64); 2]> = (0..H * W)
            .map(|i| position(&camera_from_pano(&grid_direction(i / W, i % W))))
            .collect();
        let (alpha, bad) = refine(BLEND_DEG / 2.0, true, &mask, &positions, size);
        let alpha = if bad {
            refine(FALLBACK_DEG / 2.0, false, &mask, &positions, size).0
        } else {
            alpha
        };
        Blend {
            alpha,
            mask,
            size,
            guard,
        }
    }

    /// The weight of lens 0 for the camera-frame direction `d` with the sample positions (pixels) in each lens.
    pub fn weight(&self, d: &Vec3, positions: [(f64, f64); 2]) -> f64 {
        let (r, c) = grid_position(d);
        let alpha = f64::from(bilinear(&self.alpha, W, H, c, r, true));
        // The shader samples the masks at texel centres.
        let m: [f64; 2] = std::array::from_fn(|l| {
            let (u, v) = positions[l];
            let s = MASK as f64 / self.size;
            f64::from(bilinear(
                &self.mask[l],
                MASK,
                MASK,
                (u + 0.5) * s - 0.5,
                (v + 0.5) * s - 0.5,
                false,
            ))
        });
        let (left, right) = (alpha * m[0], (1.0 - alpha) * m[1]);
        if left > 0.0 || right > 0.0 {
            left / (left + right)
        } else {
            alpha
        }
    }
}

/// The alpha map with the masks (`RefineAlphaWithFishmask`), and `true` if the ramp meets a masked part of a lens.
fn refine(
    half: f64,
    widen: bool,
    mask: &[Vec<f32>; 2],
    positions: &[[(f64, f64); 2]],
    size: f64,
) -> (Vec<f32>, bool) {
    // The template and the index of the lenses with a weight.
    let mut template = vec![0.0f64; H * W];
    for r in 0..H {
        for c in 0..W {
            template[r * W + c] = template_weight(&grid_direction(r, c), half, widen);
        }
        template[r * W] = template[r * W + 1];
        template[r * W + W - 1] = template[r * W + W - 2];
    }
    let index = |r: usize, c: usize| {
        let a = template[r * W + c];
        u8::from(a > 0.0) | u8::from(1.0 - a > 0.0) << 1
    };
    let s = MASK as f64 / size;
    let mut out = vec![0.0f32; H * W];
    let mut bad = false;
    for r in 0..H {
        for c in 0..W {
            let r1 = (r + 1).min(H - 1);
            let bits = index(r, c) | index(r, (c + 1) % W) | index(r1, c) | index(r1, (c + 1) % W);
            let a0 = if bits & 1 != 0 {
                template[r * W + c]
            } else {
                0.0
            };
            let a1 = if bits & 2 != 0 {
                1.0 - template[r * W + c]
            } else {
                0.0
            };
            let sample = |l: usize| {
                let (u, v) = positions[r * W + c][l];
                f64::from(bilinear(&mask[l], MASK, MASK, u * s, v * s, false))
            };
            let f0 = if a0 > 0.0 { sample(0) } else { 0.0 };
            let mut f1 = if a1 > 0.0 { sample(1) } else { 0.0 };
            if r < (0.9 * H as f64) as usize
                && ((a0 == 1.0 && f0 == 0.0) || (a1 == 1.0 && f1 == 0.0))
            {
                bad = true;
            }
            let mut w0 = f0 * a0;
            if w0 <= 0.0 {
                w0 = a0;
                f1 = 1.0;
            }
            let sum = w0 + a1 * f1;
            out[r * W + c] = (w0 / if sum > 0.0 { sum } else { 1.0 }) as f32;
        }
        if r as f64 > 0.8 * H as f64 {
            nadir_fix(&mut out[r * W..(r + 1) * W]);
        }
    }
    (out, bad)
}

/// Cleans one alpha-map row near the nadir: lens 1 everywhere between the first and the last 0, lens 0 from the
/// nearest 1 on each side of the centre to the ends, and straight ramps between the plateaus.
fn nadir_fix(row: &mut [f32]) {
    if let (Some(first), Some(last)) = (
        row.iter().position(|&v| v == 0.0),
        row.iter().rposition(|&v| v == 0.0),
    ) {
        row[first..=last].fill(0.0);
    }
    let centre = row.len() / 2;
    if let Some(left) = (0..centre).rev().find(|&i| row[i] == 1.0) {
        row[..=left].fill(1.0);
    }
    if let Some(right) = (centre..row.len()).find(|&i| row[i] == 1.0) {
        row[right..].fill(1.0);
    }
    let plateau = |v: f32| v == 0.0 || v == 1.0;
    let mut i = 0;
    while i < row.len() {
        if plateau(row[i]) {
            i += 1;
            continue;
        }
        let start = i - usize::from(i > 0);
        let mut end = i;
        while end < row.len() && !plateau(row[end]) {
            end += 1;
        }
        // Only a ramp from one lens to the other: near the pole, a dip between two plateaus of lens 0 stays.
        // (Filled, it gave lens 0 within 7 degrees of the camera bottom; the Studio export changes lens there.)
        if i > 0 && end < row.len() && row[start] != row[end] {
            let (a, b) = (row[start], row[end]);
            for (k, value) in row.iter_mut().enumerate().take(end).skip(start + 1) {
                *value = (a + (b - a) * (k - start) as f32 / (end - start) as f32).clamp(0.0, 1.0);
            }
        }
        i = end;
    }
}

/// The mask of one lens image (`calcFisheyeMaskONEX5AndProtector`, version < 7): a disk at the field of view of
/// lens 0's last table point (at most `max_fov`), cut in the lower half along `table`, with a soft edge.
fn lens_mask(
    table: Table,
    lens0_table: Table,
    max_fov: f64,
    size: f64,
    centre: (f64, f64),
    project: impl Fn(&Vec3) -> (f64, f64),
) -> Vec<f32> {
    let s = MASK as f64 / size;
    // The squared radius (mask pixels) of the ray at `fov` degrees from the axis, `a` degrees from straight down.
    let radius2 = |fov: f64, a: f64| {
        let (sf, cf) = fov.to_radians().sin_cos();
        let (sa, ca) = a.to_radians().sin_cos();
        let (du, dv) = project(&[sf * sa, sf * ca, cf]);
        (du * s).powi(2) + (dv * s).powi(2)
    };
    let fc = max_fov.min(lens0_table[lens0_table.len() - 1].1);
    let rc = radius2(fc, 0.0).sqrt();
    let points: Vec<(f64, f64)> = table.iter().map(|&(a, f)| (a, radius2(f, a))).collect();
    let (cx, cy) = (centre.0 * s, centre.1 * s);
    let mut mask = vec![0u8; MASK * MASK];
    for y in 0..MASK {
        for x in 0..MASK {
            if (x as f64 - cx).powi(2) + (y as f64 - cy).powi(2) <= rc * rc {
                mask[y * MASK + x] = 255;
            }
        }
    }
    // The cut: for each column near the centre, clear from the bottom up to the first pixel inside the table.
    let reach = 70f64.to_radians().sin() * rc;
    let (x0, x1) = (
        ((cx - reach).max(0.0)) as usize,
        ((cx + reach).min(MASK as f64)) as usize,
    );
    for x in x0..x1 {
        for y in (MASK / 2 + 1..MASK).rev() {
            let (dx, dy) = (x as f64 - cx, y as f64 - cy);
            let theta = dx.abs().atan2(dy.abs()).to_degrees();
            let mut rr = rc * rc;
            for k in 0..points.len().saturating_sub(1) {
                let ((a0, r0), (a1, r1)) = (points[k], points[k + 1]);
                if a0 <= theta && theta < a1 {
                    rr = r0 + (theta - a0) * (r1 - r0) / (a1 - a0);
                }
            }
            if dx * dx + dy * dy <= rr {
                break;
            }
            mask[y * MASK + x] = 0;
        }
    }
    for i in 0..MASK {
        mask[i] = 0;
        mask[i * MASK] = 0;
        mask[i * MASK + MASK - 1] = 0;
    }
    distance_transform(&mask)
        .iter()
        .map(|&d| (d * FEATHER).min(1.0))
        .collect()
}

/// OpenCV `distanceTransform(DIST_L2, 5)`: the 5 x 5 chamfer distance (1, 1.4, 2.1969) of each non-zero pixel to the
/// nearest zero pixel.
fn distance_transform(mask: &[u8]) -> Vec<f32> {
    const A: f32 = 1.0;
    const B: f32 = 1.4;
    const C: f32 = 2.1969;
    let n = MASK as isize;
    let mut d: Vec<f32> = mask
        .iter()
        .map(|&v| if v == 0 { 0.0 } else { f32::INFINITY })
        .collect();
    let forward = [
        (-1, 0, A),
        (0, -1, A),
        (-1, -1, B),
        (1, -1, B),
        (-1, -2, C),
        (1, -2, C),
        (-2, -1, C),
        (2, -1, C),
    ];
    for y in 0..n {
        for x in 0..n {
            let i = (y * n + x) as usize;
            if d[i] == 0.0 {
                continue;
            }
            for &(dx, dy, w) in &forward {
                let (xx, yy) = (x + dx, y + dy);
                if xx >= 0 && xx < n && yy >= 0 {
                    d[i] = d[i].min(d[(yy * n + xx) as usize] + w);
                }
            }
        }
    }
    for y in (0..n).rev() {
        for x in (0..n).rev() {
            let i = (y * n + x) as usize;
            if d[i] == 0.0 {
                continue;
            }
            for &(dx, dy, w) in &forward {
                let (xx, yy) = (x - dx, y - dy);
                if xx >= 0 && xx < n && yy < n {
                    d[i] = d[i].min(d[(yy * n + xx) as usize] + w);
                }
            }
        }
    }
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_template_is_a_linear_ramp_of_6_degrees_that_widens_near_the_nadir() {
        let at = |theta: f64, z: f64| {
            // A direction at `theta` degrees from the lens 0 axis, with height z.
            let (s, c) = theta.to_radians().sin_cos();
            let h = (s * s - z * z).max(0.0).sqrt();
            template_weight(&[c, h, z], BLEND_DEG / 2.0, true)
        };
        assert_eq!(at(86.9, 0.0), 1.0);
        assert!((at(90.0, 0.0) - 0.5).abs() < 1e-9);
        assert!((at(88.5, 0.0) - 0.75).abs() < 1e-9);
        assert_eq!(at(93.1, 0.0), 0.0);
        // At 85 degrees south, the ramp is 15 degrees wide: 1.5 degrees from the seam is 0.6.
        assert!((at(88.5, -0.99) - 0.6).abs() < 1e-9);
    }

    #[test]
    fn the_nadir_fix_makes_straight_ramps_between_plateaus() {
        let mut row = vec![1.0, 1.0, 0.9, 0.2, 0.0, 0.3, 0.0, 0.0, 0.5, 1.0];
        nadir_fix(&mut row);
        assert_eq!(&row[4..8], &[0.0; 4]);
        assert!(
            (row[2] - 2.0 / 3.0).abs() < 1e-6 && (row[3] - 1.0 / 3.0).abs() < 1e-6,
            "{row:?}"
        );
        assert!((row[8] - 0.5).abs() < 1e-6, "{row:?}");
        // Near the pole: no 0, and a dip between two plateaus of lens 0 is not a ramp.
        let mut pole = vec![1.0, 1.0, 0.8, 0.3, 0.1, 0.3, 0.8, 1.0];
        let before = pole.clone();
        nadir_fix(&mut pole);
        assert_eq!(pole, before);
    }

    #[test]
    fn the_distance_transform_is_the_opencv_chamfer() {
        let mut mask = vec![255u8; MASK * MASK];
        mask[400 * MASK + 400] = 0;
        let d = distance_transform(&mask);
        assert_eq!(d[400 * MASK + 401], 1.0);
        assert_eq!(d[401 * MASK + 401], 1.4);
        assert_eq!(d[402 * MASK + 401], 2.1969);
        assert!((d[400 * MASK + 403] - 3.0).abs() < 1e-6);
    }
}
