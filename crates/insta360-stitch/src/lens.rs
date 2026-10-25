//! This module contains the dual-fisheye lens model and the equirectangular projection, one row at a time.
//!
//! The calibration comes from the file. As Insta360 (`RenderModelType::GetOffsetInUsed`), the model uses the v6
//! calibration if the file has it: a unified (MEI) projection with 13 distortion terms for each lens.
//! Without v6, it uses the first calibration: the radius, center and rotation of each lens, and a fourth-degree
//! polynomial for each lens type that changes the angle to a radius.
//! At the seam, an S-curve blends the two lenses across 18 degrees. The blend is narrower near the nadir.
//! We compared all constants with exports from Insta360 Studio.

use crate::blend::{template_weight_camera, Blend};
use crate::error::{unsupported, Result};
use crate::math::*;
use crate::trailer::Trailer;

/// Gives the angle (degrees) -> radius polynomial for the lens type of the calibration.
fn lens_coefficients(lens_type: u32) -> Option<[f64; 5]> {
    match lens_type {
        113 => Some([0.0, 0.03159, -8.415e-5, 2.201e-6, -1.284e-8]), // X5
        _ => None,
    }
}

/// Gives the half field of view (degrees) for the state of the lens guard or accessory.
/// Without a guard, the value agrees with the 200 degree field of view of the lens type.
/// For the premium guard, we measured the value from Studio exports (97.43, 97.56, 97.45).
/// We did not measure the standard guard, diving housings or ND filters. Thus, the library refuses them.
fn half_fov(offset_state: u64) -> Option<f64> {
    match offset_state {
        0 => Some(100.0), // No guard
        5 => Some(97.5),  // Premium guard
        _ => None,
    }
}

/// Insta360 (`FlowstateRender2`) samples a lens up to this angle from its axis with the v6 calibration.
const MEI_MAX_ANGLE_DEG: f64 = 120.0;
/// The offset state of the premium lens guard.
const PREMIUM_GUARD: u64 = 5;
/// Lens type 115, the design that Insta360 uses for an X5 with the premium lens guard (`Lens::getV6distort`):
/// xi, the radial terms d0..d4 and the focal length (mm).
const GUARD_XI: f64 = 2.0;
const GUARD_RADIAL: [f64; 5] = [0.2701227043, 1.213889541, 1.572668844, -10.02050387, 0.0];
const GUARD_FOCAL_MM: f64 = 5.327691706;

#[derive(Debug, Clone, Copy)]
pub struct Lens {
    pub rotation: Mat3,
    pub cx: f64,
    pub cy: f64,
    /// The radius at `half_fov` for the polynomial. Not used with `mei`.
    pub radius: f64,
    /// The v6 projection. `None`: the polynomial of the lens type.
    pub mei: Option<Mei>,
}

/// The v6 projection of one lens, scaled to the lens crop. For a lens ray P (the axis is +z):
/// x = X / (Z + xi |P|), y = Y / (Z + xi |P|), and u = cx + fx xd, v = cy + fy yd, with (xd, yd) from
/// [`Mei::distort`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Mei {
    pub xi: f64,
    pub fx: f64,
    pub fy: f64,
    pub distortion: [f64; 13],
}

impl Mei {
    /// Gives (xd, yd) for the projected point (x, y), as Insta360: radial terms d0..d4, tangential terms
    /// A = d5 + d7 r^2 and B = d6 + d8 r^2, and thin-prism terms d9..d12.
    pub fn distort(&self, x: f64, y: f64) -> (f64, f64) {
        let d = &self.distortion;
        let r2 = x * x + y * y;
        let radial = 1.0 + r2 * (d[0] + r2 * (d[1] + r2 * (d[2] + r2 * (d[3] + r2 * d[4]))));
        let (a, b) = (d[5] + d[7] * r2, d[6] + d[8] * r2);
        (
            x * radial + a * (r2 + 2.0 * x * x) + 2.0 * x * y * b + d[9] * r2 + d[11] * r2 * r2,
            y * radial + b * (r2 + 2.0 * y * y) + 2.0 * x * y * a + d[10] * r2 + d[12] * r2 * r2,
        )
    }

    /// Changes the lens to the lens-guard design, as Insta360 (`ins::OffsetConvert::convertOffset`, type 0x35).
    /// The pixels per mm come from a least-squares fit of the projection of this lens to `mm`, the angle (degrees)
    /// -> radius (mm) polynomial of the lens type, from 0 to 100 degrees in steps of 0.1 degree. The centre, the
    /// rotation and the terms d5..d12 do not change.
    fn fit_guard(&mut self, mm: impl Fn(f64) -> f64) {
        let (f, d) = ((self.fx * self.fy).sqrt(), &self.distortion);
        let (mut sum_xy, mut sum_yy, mut t) = (0.0, 0.0, 0.0f64);
        while t < 100.0 {
            let r = mm(t);
            let x = t.to_radians().sin() / (t.to_radians().cos() + self.xi);
            let x2 = x * x;
            let xd = x * (1.0 + x2 * (d[0] + x2 * (d[1] + x2 * (d[2] + x2 * (d[3] + x2 * d[4])))));
            sum_xy += f * xd * r;
            sum_yy += r * r;
            t += 0.1;
        }
        let focal = sum_xy / sum_yy * GUARD_FOCAL_MM;
        (self.xi, self.fx, self.fy) = (GUARD_XI, focal, focal);
        self.distortion[..5].copy_from_slice(&GUARD_RADIAL);
    }

    /// Gives the position of the lens ray `p` relative to the lens centre (pixels).
    fn project(&self, p: &Vec3) -> (f64, f64) {
        let z = p[2] + self.xi * norm(p);
        let (xd, yd) = self.distort(p[0] / z, p[1] / z);
        (self.fx * xd, self.fy * yd)
    }
}

/// The two lenses, scaled to a lens crop of `lens_size` pixels and levelled by `level`.
#[derive(Debug, Clone)]
pub struct LensModel {
    pub lenses: [Lens; 2],
    pub coefficients: [f64; 5],
    pub half_fov: f64,
    pub level: Mat3,
    /// The seam blend of the Studio export (alpha map and lens masks). `None`: the template ramp only.
    pub blend: Option<std::sync::Arc<Blend>>,
}

/// The sample position in each lens for one output row, and the blend weight of lens 0.
/// A position outside a lens is NaN.
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
    /// Makes the model for the calibration of a trailer.
    /// It refuses cameras, lens guards and calibrations that it cannot stitch.
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
        let p = mul(&axis_rotation('x', 90.0), &axis_rotation('y', 90.0));
        let rotation = |l: usize| {
            let [rx, ry, rz] = trailer.lens_angles(l);
            let lens = rotate_matrix(rx, ry, rz);
            let lens = if l == 1 {
                mul(&lens, &axis_rotation('z', 180.0))
            } else {
                lens
            };
            mul(&mul(&lens, &p), &level)
        };
        if let Some(v6) = &trailer.offset_v6 {
            let lenses = std::array::from_fn(|l| {
                let c = &v6[l];
                // The calibration frame has the two lenses side by side.
                let (mut width, mut height) = (c[24] / 2.0, c[25]);
                let (mut cx, mut cy) = (if l == 1 { c[3] - width } else { c[3] }, c[4]);
                let mut mei = Mei {
                    xi: c[0],
                    fx: c[1],
                    fy: c[2],
                    distortion: std::array::from_fn(|k| c[11 + k]),
                };
                if trailer.offset_state == PREMIUM_GUARD {
                    let poly = |t: f64| {
                        coefficients[0]
                            + t * (coefficients[1]
                                + t * (coefficients[2]
                                    + t * (coefficients[3] + t * coefficients[4])))
                    };
                    mei.fit_guard(poly);
                }
                // The image shows a part of the calibration frame (`convertOffset` with the crop).
                if let Some([a, b, w, h, x, y]) = trailer.window_crop {
                    if width * b == height * a {
                        let k = a / width;
                        cx = k * (cx + 0.5) - 0.5 - ((a - w) / 2.0 - x);
                        cy = k * (cy + 0.5) - 0.5 - ((b - h) / 2.0 - y);
                        (mei.fx, mei.fy, width, height) = (mei.fx * k, mei.fy * k, w, h);
                    }
                }
                // Insta360 scales a position by (size - 1) / (calibration size - 1).
                let (sx, sy) = (
                    (lens_size - 1.0) / (width - 1.0),
                    (lens_size - 1.0) / (height - 1.0),
                );
                (mei.fx, mei.fy) = (mei.fx * sx, mei.fy * sy);
                Lens {
                    rotation: rotation(l),
                    cx: cx * sx,
                    cy: cy * sy,
                    radius: 0.0,
                    mei: Some(mei),
                }
            });
            // Studio converts only the v6 calibration to the lens-guard design (lens type 115).
            let guard = trailer.offset_state == PREMIUM_GUARD;
            return Ok(LensModel {
                lenses,
                coefficients,
                half_fov: MEI_MAX_ANGLE_DEG,
                level,
                blend: None,
            }
            .with_blend(lens_size, guard));
        }
        let scale = 2.0 * lens_size / calib_width;
        let lenses = [
            Lens {
                rotation: rotation(0),
                cx: o[2] * scale,
                cy: o[3] * scale,
                radius: r0 * scale,
                mei: None,
            },
            Lens {
                rotation: rotation(1),
                // The center of lens 1 is a position in the full frame, with the two lenses side by side.
                cx: o[8] * scale - lens_size,
                cy: o[9] * scale,
                radius: r1 * scale,
                mei: None,
            },
        ];
        Ok(LensModel {
            lenses,
            coefficients,
            half_fov,
            level,
            blend: None,
        }
        .with_blend(lens_size, false))
    }

    /// Adds the seam blend of the Studio export for lens images of `size` pixels.
    fn with_blend(mut self, size: f64, guard: bool) -> Self {
        let unlevel = transpose(&self.level);
        let centre = [
            (self.lenses[0].cx, self.lenses[0].cy),
            (self.lenses[1].cx, self.lenses[1].cy),
        ];
        let blend = Blend::new(
            size,
            centre,
            guard,
            |l, ray| self.offset(l, ray),
            |d| {
                let s = self.ray(&apply(&unlevel, d));
                [(s.u[0], s.v[0]), (s.u[1], s.v[1])]
            },
        );
        self.blend = Some(std::sync::Arc::new(blend));
        self
    }

    /// Gives the position (pixels, from the lens centre) of a ray in the frame of lens `l` (axis +z).
    pub(crate) fn offset(&self, l: usize, ray: &Vec3) -> (f64, f64) {
        let lens = &self.lenses[l];
        match &lens.mei {
            Some(mei) => mei.project(ray),
            None => {
                let [rx, ry, rz] = *ray;
                let phi = rx.hypot(ry).atan2(rz).to_degrees();
                let r = self.map_poly(phi) / self.map_poly(self.half_fov) * lens.radius;
                let theta = ry.atan2(rx);
                (r * theta.cos(), r * theta.sin())
            }
        }
    }

    fn map_poly(&self, phi: f64) -> f64 {
        let c = &self.coefficients;
        c[0] + phi * (c[1] + phi * (c[2] + phi * (c[3] + phi * c[4])))
    }

    /// Gives the position of the ray `pos` in each lens, and the seam weight of lens 0.
    /// `pos` is a unit direction in the output frame; the model applies its `level` rotation.
    pub fn ray(&self, pos: &Vec3) -> RaySample {
        let mut sample = RaySample {
            u: [0.0; 2],
            v: [0.0; 2],
            phi: [0.0; 2],
            alpha: 0.0,
        };
        for (l, lens) in self.lenses.iter().enumerate() {
            let ray = apply(&lens.rotation, pos);
            let phi = ray[0].hypot(ray[1]).atan2(ray[2]).to_degrees();
            let (du, dv) = self.offset(l, &ray);
            sample.u[l] = du + lens.cx;
            sample.v[l] = dv + lens.cy;
            sample.phi[l] = phi;
        }
        // The seam blend on the level (camera-frame) ray: 1 is lens 0, 0 is lens 1.
        let d = apply(&self.level, pos);
        sample.alpha = match &self.blend {
            Some(blend) => {
                blend.weight(&d, [(sample.u[0], sample.v[0]), (sample.u[1], sample.v[1])])
            }
            None => template_weight_camera(&d),
        };
        sample
    }

    /// Fills `maps` for row `y` of an equirectangular output of `width` x `height`.
    pub fn row(&self, y: usize, width: usize, height: usize, maps: &mut RowMaps) {
        for x in 0..width {
            let sample = self.ray(&output_direction(x, y, width, height));
            for l in 0..2 {
                let visible = sample.phi[l] <= self.half_fov;
                maps.u[l][x] = if visible {
                    sample.u[l] as f32
                } else {
                    f32::NAN
                };
                maps.v[l][x] = if visible {
                    sample.v[l] as f32
                } else {
                    f32::NAN
                };
            }
            maps.weight0[x] = sample.alpha as f32;
        }
    }
}

/// The direction of the centre of output pixel (x, y) of an equirectangular output of `width` x `height`.
pub fn output_direction(x: usize, y: usize, width: usize, height: usize) -> Vec3 {
    use std::f64::consts::PI;
    let (sp, cp) = (PI * (y as f64 + 0.5) / height as f64).sin_cos();
    let (st, ct) = (2.0 * PI * (x as f64 + 0.5) / width as f64).sin_cos();
    [-sp * st, cp, sp * ct]
}

/// The position of one ray in the two lenses. See [`LensModel::ray`].
#[derive(Debug, Clone, Copy)]
pub struct RaySample {
    /// The position in each lens crop (pixels). Outside the field of view, the polynomial continues.
    pub u: [f64; 2],
    pub v: [f64; 2],
    /// The angle between the ray and the axis of each lens (degrees). The lens sees the ray up to `half_fov`.
    pub phi: [f64; 2],
    /// The seam weight of lens 0, before the check that each lens sees the ray.
    pub alpha: f64,
}

/// Gives the weight of lens 0 for a pixel.
/// If the two lenses see the pixel, the weight is the seam weight. If only one lens sees it, that lens gets the pixel.
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
