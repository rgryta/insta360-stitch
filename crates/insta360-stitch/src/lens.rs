//! The dual-fisheye lens model and the equirectangular projection, row by row.
//!
//! Calibration comes from the file itself: per-lens radius, centre and rotation. The lens maps
//! angle to radius with a quartic polynomial per lens type; the seam is a smooth S-curve blend
//! across an 18 degree band, narrowed towards the nadir. All constants were matched against
//! Insta360 Studio's exports.

use crate::error::{unsupported, Result};
use crate::math::*;
use crate::trailer::Trailer;

/// Angle (degrees) -> radius polynomial, by the calibration's lens type.
fn lens_coefficients(lens_type: u32) -> Option<[f64; 5]> {
    match lens_type {
        113 => Some([0.0, 0.03159, -8.415e-5, 2.201e-6, -1.284e-8]), // X5
        _ => None,
    }
}

/// Half field of view (degrees) by lens guard / accessory state. A bare lens matches the lens
/// type's 200 degree FOV; the premium guard is fitted to Studio exports (97.43 / 97.56 / 97.45).
/// The standard guard, diving housings and ND filters are unmeasured, so they are refused.
fn half_fov(offset_state: u64) -> Option<f64> {
    match offset_state {
        0 => Some(100.0), // no guard
        5 => Some(97.5),  // premium guard
        _ => None,
    }
}

const BLEND_ANGLE_DEG: f64 = 18.0;
const NADIR_BLEND_FLOOR_DEG: f64 = 5.0;
// the shader works in f32
const ALPHA_LAMBDA: f64 = 5.2_f32 as f64;

#[derive(Debug, Clone, Copy)]
pub struct Lens {
    pub rotation: Mat3,
    pub cx: f64,
    pub cy: f64,
    pub radius: f64,
}

/// Both lenses scaled to a lens crop of `lens_size` pixels, levelled by `level`.
#[derive(Debug, Clone)]
pub struct LensModel {
    pub lenses: [Lens; 2],
    pub coefficients: [f64; 5],
    pub half_fov: f64,
    pub level: Mat3,
}

/// Where each lens is sampled for one output row (NaN outside that lens), and lens 0's blend weight.
#[derive(Debug, Clone)]
pub struct RowMaps {
    pub u: [Vec<f32>; 2],
    pub v: [Vec<f32>; 2],
    pub weight0: Vec<f32>,
}

impl RowMaps {
    pub fn new(width: usize) -> Self {
        RowMaps {
            u: [vec![0.0; width], vec![0.0; width]],
            v: [vec![0.0; width], vec![0.0; width]],
            weight0: vec![0.0; width],
        }
    }
}

impl LensModel {
    /// The model for a trailer's calibration; refuses cameras, lens guards and calibrations it can't stitch.
    pub fn new(trailer: &Trailer, lens_size: usize, level: Mat3) -> Result<Self> {
        let o = &trailer.offset;
        let lens_word = o[15] as u32;
        let Some(coefficients) = lens_coefficients(lens_word & 0xff) else {
            return unsupported(format!(
                "unsupported lens type {} (only the Insta360 X5 is supported)",
                lens_word & 0xff
            ));
        };
        let Some(half_fov) = half_fov(trailer.offset_state) else {
            return unsupported(format!(
                "unsupported lens guard or accessory (offset state {})",
                trailer.offset_state
            ));
        };
        let (r0, r1, calib_width) = (o[1], o[7], o[13]);
        if !(calib_width > 0.0 && r0 > 0.0 && r1 > 0.0) {
            return unsupported("invalid lens calibration");
        }
        let lens_size = lens_size as f64;
        let scale = 2.0 * lens_size / calib_width;
        let p = mul(&axis_rotation('x', 90.0), &axis_rotation('y', 90.0));
        let lenses = [
            Lens {
                rotation: mul(&mul(&rotate_matrix(o[4], o[5], o[6]), &p), &level),
                cx: o[2] * scale,
                cy: o[3] * scale,
                radius: r0 * scale,
            },
            Lens {
                rotation: mul(
                    &mul(
                        &mul(
                            &rotate_matrix(o[10], o[11], o[12]),
                            &axis_rotation('z', 180.0),
                        ),
                        &p,
                    ),
                    &level,
                ),
                // lens 1's centre is given in the full side-by-side frame
                cx: o[8] * scale - lens_size,
                cy: o[9] * scale,
                radius: r1 * scale,
            },
        ];
        Ok(LensModel {
            lenses,
            coefficients,
            half_fov,
            level,
        })
    }

    fn map_poly(&self, phi: f64) -> f64 {
        let c = &self.coefficients;
        c[0] + phi * (c[1] + phi * (c[2] + phi * (c[3] + phi * c[4])))
    }

    /// Fills `maps` for row `y` of a `width` x `height` equirectangular output.
    pub fn row(&self, y: usize, width: usize, height: usize, maps: &mut RowMaps) {
        use std::f64::consts::PI;
        let map_max = self.map_poly(self.half_fov);
        let ph = PI * (y as f64 + 0.5) / height as f64;
        let (sp, cp) = ph.sin_cos();
        for x in 0..width {
            let th = 2.0 * PI * (x as f64 + 0.5) / width as f64;
            let (st, ct) = th.sin_cos();
            let pos = [-sp * st, cp, sp * ct];
            for (l, lens) in self.lenses.iter().enumerate() {
                let [rx, ry, rz] = apply(&lens.rotation, &pos);
                let phi = rx.hypot(ry).atan2(rz).to_degrees();
                if phi > self.half_fov {
                    maps.u[l][x] = f32::NAN;
                    maps.v[l][x] = f32::NAN;
                    continue;
                }
                let r = self.map_poly(phi) / map_max * lens.radius;
                let theta = ry.atan2(rx);
                maps.u[l][x] = (r * theta.cos() + lens.cx) as f32;
                maps.v[l][x] = (r * theta.sin() + lens.cy) as f32;
            }
            // seam blend on the levelled ray: 1 -> lens 0, 0 -> lens 1
            let [px, py, pz] = apply(&self.level, &pos);
            let blend = if py < 0.0 {
                ((1.0 - py.powi(4)) * BLEND_ANGLE_DEG).clamp(NADIR_BLEND_FLOOR_DEG, BLEND_ANGLE_DEG)
            } else {
                BLEND_ANGLE_DEG
            };
            let phi_b = px.hypot(py).atan2(-pz).to_degrees();
            let t = ((90.0 - phi_b + blend / 2.0) / blend).clamp(0.0, 1.0);
            maps.weight0[x] = if t <= 0.5 {
                0.5 * (2.0 * t).powf(ALPHA_LAMBDA)
            } else {
                1.0 - 0.5 * (2.0 * (1.0 - t)).powf(ALPHA_LAMBDA)
            } as f32;
        }
    }
}

/// Lens 0's share of a pixel: the seam weight where both lenses see it, the only lens that does otherwise.
pub fn blend_weight(valid0: bool, valid1: bool, alpha: f32) -> f32 {
    let w0 = if valid0 { alpha } else { 0.0 };
    let w1 = if valid1 { 1.0 - alpha } else { 0.0 };
    if w0 + w1 > 0.0 {
        w0 / (w0 + w1)
    } else if valid0 {
        1.0
    } else {
        0.0
    }
}
