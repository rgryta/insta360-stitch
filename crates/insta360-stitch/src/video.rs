//! This module stitches the frames of a video one at a time, with a different rotation for each frame.
//!
//! The lens model is slow: each ray needs trigonometry and a polynomial. Thus, the module calculates it one
//! time, for the directions of a cube map in the camera frame. For each frame, it rotates the output direction,
//! reads the cube map (bilinear) and samples the two lenses. A frame then needs no trigonometry per pixel.
//! Insta360 Studio uses the same design on the GPU: a lens lookup map, and a rotation matrix for each frame.

use crate::error::{invalid, unsupported, Result};
use crate::lens::{blend_weight, LensModel};
use crate::math::Mat3;
use crate::stitch::MAX_VIDEO_WIDTH;

#[cfg(feature = "parallel")]
use rayon::prelude::*;

/// The cells on one edge of a cube face. One cell is 0.18 degrees.
const FACE: usize = 512;
/// The face size with a border of one cell, thus the bilinear interpolation never leaves the face.
const SIDE: usize = FACE + 2;
/// The lens coordinates are stored in 1/8 pixel.
const SUBPIXEL: f32 = 8.0;
/// A position more than this outside the lens crop is not used (the lens does not see it).
const COORD_MARGIN: f64 = 64.0;
/// The largest lens stream that the i16 coordinates can hold.
const MAX_LENS_SIZE: usize = (i16::MAX as usize) / 8 - 2 * COORD_MARGIN as usize;
/// The field-of-view margin is stored in 0.1 degree.
const MARGIN_SCALE: f64 = 10.0;

/// One cell of the lens table: the position in each lens, the seam weight of lens 0, and how far inside the
/// field of view of each lens the direction is.
#[derive(Debug, Clone, Copy, Default)]
struct Cell {
    u: [i16; 2],
    v: [i16; 2],
    alpha: u8,
    margin: [i8; 2],
}

/// The direction of cell (s, t) of a face, s and t in -1..1 (outside for the border). Faces: +x, -x, +y, -y, +z, -z.
fn face_direction(face: usize, s: f64, t: f64) -> [f64; 3] {
    match face {
        0 => [1.0, t, -s],
        1 => [-1.0, t, s],
        2 => [s, 1.0, -t],
        3 => [s, -1.0, t],
        4 => [s, t, 1.0],
        _ => [-s, t, -1.0],
    }
}

/// The face and the (s, t) position of a direction: the inverse of `face_direction`.
#[inline(always)]
fn face_of(d: [f32; 3]) -> (usize, f32, f32) {
    let (ax, ay, az) = (d[0].abs(), d[1].abs(), d[2].abs());
    if ax >= ay && ax >= az {
        if d[0] > 0.0 {
            (0, -d[2] / ax, d[1] / ax)
        } else {
            (1, d[2] / ax, d[1] / ax)
        }
    } else if ay >= az {
        if d[1] > 0.0 {
            (2, d[0] / ay, -d[2] / ay)
        } else {
            (3, d[0] / ay, d[2] / ay)
        }
    } else if d[2] > 0.0 {
        (4, d[0] / az, d[1] / az)
    } else {
        (5, -d[0] / az, d[1] / az)
    }
}

/// The interpolated lens table at one direction: lens positions (pixels), seam weight, field-of-view margins.
struct Lookup {
    u: [f32; 2],
    v: [f32; 2],
    alpha: f32,
    margin: [f32; 2],
}

/// Stitches the frames of a video. See the module documentation.
pub struct FrameStitcher {
    table: Vec<Cell>,
    lens_size: usize,
    width: usize,
    /// sin and cos of the polar angle of each luma row, and of the azimuth of each luma column.
    rows: Vec<(f32, f32)>,
    cols: Vec<(f32, f32)>,
    /// The same for the chroma samples (4:2:0, left siting: horizontally at the even luma column).
    chroma_rows: Vec<(f32, f32)>,
    chroma_cols: Vec<(f32, f32)>,
}

impl FrameStitcher {
    /// Makes the lens table for lens streams of `lens_size` x `lens_size` and an output of `width` x `width / 2`.
    /// Make `model` for this lens size and with the identity as level rotation: the table is in the camera frame.
    pub fn new(model: &LensModel, lens_size: usize, width: usize) -> Result<Self> {
        if model.level != crate::math::IDENTITY {
            // A level rotation in the model and the frame rotation would turn the image two times.
            return invalid(
                "make the model of a frame stitcher with the identity as level rotation",
            );
        }
        if lens_size < 8 || !lens_size.is_multiple_of(2) || lens_size > MAX_LENS_SIZE {
            return unsupported(format!("unsupported lens stream size {lens_size}"));
        }
        if width == 0 || !width.is_multiple_of(4) || width > MAX_VIDEO_WIDTH {
            return invalid(format!(
                "the output width must be a multiple of 4 and at most {MAX_VIDEO_WIDTH}, not {width}"
            ));
        }
        let size = lens_size as f64;
        let quantize = |value: f64| {
            (value.clamp(-COORD_MARGIN, size + COORD_MARGIN) * SUBPIXEL as f64).round() as i16
        };
        let cell = |k: usize| {
            let (face, rest) = (k / (SIDE * SIDE), k % (SIDE * SIDE));
            let (j, i) = (rest / SIDE, rest % SIDE);
            // Index 1..=FACE is the face, index 0 and FACE + 1 are the border.
            let s = (i as f64 - 0.5) / FACE as f64 * 2.0 - 1.0;
            let t = (j as f64 - 0.5) / FACE as f64 * 2.0 - 1.0;
            let d = face_direction(face, s, t);
            let n = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
            let ray = model.ray(&[d[0] / n, d[1] / n, d[2] / n]);
            let margin = |l: usize| {
                ((model.half_fov - ray.phi[l]) * MARGIN_SCALE)
                    .clamp(-127.0, 127.0)
                    .round() as i8
            };
            Cell {
                u: [quantize(ray.u[0]), quantize(ray.u[1])],
                v: [quantize(ray.v[0]), quantize(ray.v[1])],
                alpha: (ray.alpha * 255.0).round() as u8,
                margin: [margin(0), margin(1)],
            }
        };
        #[cfg(feature = "parallel")]
        let table: Vec<Cell> = (0..6 * SIDE * SIDE).into_par_iter().map(cell).collect();
        #[cfg(not(feature = "parallel"))]
        let table: Vec<Cell> = (0..6 * SIDE * SIDE).map(cell).collect();

        let height = width / 2;
        let polar = |y: f64, h: usize| {
            let (s, c) = (std::f64::consts::PI * y / h as f64).sin_cos();
            (s as f32, c as f32)
        };
        let azimuth = |x: f64, w: usize| {
            let (s, c) = (2.0 * std::f64::consts::PI * x / w as f64).sin_cos();
            (s as f32, c as f32)
        };
        Ok(FrameStitcher {
            table,
            lens_size,
            width,
            rows: (0..height).map(|y| polar(y as f64 + 0.5, height)).collect(),
            cols: (0..width).map(|x| azimuth(x as f64 + 0.5, width)).collect(),
            chroma_rows: (0..height / 2)
                .map(|y| polar(2.0 * y as f64 + 1.0, height))
                .collect(),
            chroma_cols: (0..width / 2)
                .map(|x| azimuth(2.0 * x as f64 + 0.5, width))
                .collect(),
        })
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.width / 2
    }

    /// The size of one input frame: yuv420p, the two lens streams stacked (lens 0 on top), `size` x `2 * size`.
    pub fn input_frame_bytes(&self) -> usize {
        self.lens_size * self.lens_size * 3
    }

    /// The size of one output frame: yuv420p, `width` x `width / 2`.
    pub fn output_frame_bytes(&self) -> usize {
        self.width * self.width / 2 * 3 / 2
    }

    #[inline(always)]
    fn lookup(&self, d: [f32; 3]) -> Lookup {
        let (face, s, t) = face_of(d);
        // Cell centre i (1..=FACE) is at s = (i - 0.5) / FACE * 2 - 1.
        let x = (s + 1.0) * 0.5 * FACE as f32 + 0.5;
        let y = (t + 1.0) * 0.5 * FACE as f32 + 0.5;
        let (xi, yi) = ((x as usize).min(SIDE - 2), (y as usize).min(SIDE - 2));
        let (fx, fy) = (x - xi as f32, y - yi as f32);
        let base = face * SIDE * SIDE + yi * SIDE + xi;
        let cells = [
            self.table[base],
            self.table[base + 1],
            self.table[base + SIDE],
            self.table[base + SIDE + 1],
        ];
        let w = [
            (1.0 - fx) * (1.0 - fy),
            fx * (1.0 - fy),
            (1.0 - fx) * fy,
            fx * fy,
        ];
        let mut acc = [0f32; 7];
        for (cell, wt) in cells.iter().zip(w) {
            acc[0] += wt * cell.u[0] as f32;
            acc[1] += wt * cell.v[0] as f32;
            acc[2] += wt * cell.u[1] as f32;
            acc[3] += wt * cell.v[1] as f32;
            acc[4] += wt * cell.alpha as f32;
            acc[5] += wt * cell.margin[0] as f32;
            acc[6] += wt * cell.margin[1] as f32;
        }
        Lookup {
            u: [acc[0] / SUBPIXEL, acc[2] / SUBPIXEL],
            v: [acc[1] / SUBPIXEL, acc[3] / SUBPIXEL],
            alpha: acc[4] / 255.0,
            margin: [acc[5], acc[6]],
        }
    }

    /// Stitches one frame. `rotation` is the output direction -> camera rotation of the frame
    /// (see [`crate::frame_orientations`]). `src` has `input_frame_bytes`, `out` has `output_frame_bytes`.
    pub fn stitch_yuv420(&self, rotation: &Mat3, src: &[u8], out: &mut [u8]) -> Result<()> {
        if src.len() != self.input_frame_bytes() || out.len() != self.output_frame_bytes() {
            return invalid(format!(
                "a frame must have {} input bytes and {} output bytes, not {} and {}",
                self.input_frame_bytes(),
                self.output_frame_bytes(),
                src.len(),
                out.len()
            ));
        }
        if rotation.iter().flatten().any(|v| !v.is_finite()) {
            return invalid("the frame rotation is not finite");
        }
        let r: [[f32; 3]; 3] =
            std::array::from_fn(|i| std::array::from_fn(|j| rotation[i][j] as f32));
        let size = self.lens_size;
        let (luma, chroma) = src.split_at(size * size * 2);
        let (cb, cr) = chroma.split_at(size * size / 2);
        let (w, h) = (self.width, self.height());
        let (out_luma, out_chroma) = out.split_at_mut(w * h);
        let (out_cb, out_cr) = out_chroma.split_at_mut(w * h / 4);

        let luma_lens = [&luma[..size * size], &luma[size * size..]];
        let luma_row = |y: usize, row: &mut [u8]| {
            let (sp, cp) = self.rows[y];
            for (x, px) in row.iter_mut().enumerate() {
                let e = self.ray(&r, sp, cp, self.cols[x]);
                let weight = blend_weight(e.margin[0] > 0.0, e.margin[1] > 0.0, e.alpha);
                let a = sample(luma_lens[0], size, e.u[0], e.v[0]);
                let b = sample(luma_lens[1], size, e.u[1], e.v[1]);
                *px = (a * weight + b * (1.0 - weight) + 0.5) as u8;
            }
        };
        let half = size / 2;
        let cb_lens = [&cb[..half * half], &cb[half * half..]];
        let cr_lens = [&cr[..half * half], &cr[half * half..]];
        let chroma_row = |y: usize, (row_b, row_r): (&mut [u8], &mut [u8])| {
            let (sp, cp) = self.chroma_rows[y];
            for x in 0..row_b.len() {
                let e = self.ray(&r, sp, cp, self.chroma_cols[x]);
                let weight = blend_weight(e.margin[0] > 0.0, e.margin[1] > 0.0, e.alpha);
                // Luma pixel centre (u, v) is at chroma (u / 2, (v - 0.5) / 2) with left siting.
                let (u0, v0) = (e.u[0] * 0.5, (e.v[0] - 0.5) * 0.5);
                let (u1, v1) = (e.u[1] * 0.5, (e.v[1] - 0.5) * 0.5);
                for (row, lens) in [(&mut *row_b, cb_lens), (&mut *row_r, cr_lens)] {
                    let a = sample(lens[0], half, u0, v0);
                    let b = sample(lens[1], half, u1, v1);
                    row[x] = (a * weight + b * (1.0 - weight) + 0.5) as u8;
                }
            }
        };
        #[cfg(feature = "parallel")]
        {
            out_luma
                .par_chunks_mut(w)
                .enumerate()
                .for_each(|(y, row)| luma_row(y, row));
            out_cb
                .par_chunks_mut(w / 2)
                .zip(out_cr.par_chunks_mut(w / 2))
                .enumerate()
                .for_each(|(y, rows)| chroma_row(y, rows));
        }
        #[cfg(not(feature = "parallel"))]
        {
            for (y, row) in out_luma.chunks_mut(w).enumerate() {
                luma_row(y, row);
            }
            for (y, rows) in out_cb
                .chunks_mut(w / 2)
                .zip(out_cr.chunks_mut(w / 2))
                .enumerate()
            {
                chroma_row(y, rows);
            }
        }
        Ok(())
    }

    /// The table at the output direction (sin/cos of the polar angle, sin/cos of the azimuth), rotated.
    #[inline(always)]
    fn ray(&self, r: &[[f32; 3]; 3], sp: f32, cp: f32, (st, ct): (f32, f32)) -> Lookup {
        // The same output direction as `LensModel::row`.
        let p = [-sp * st, cp, sp * ct];
        let d = [
            r[0][0] * p[0] + r[0][1] * p[1] + r[0][2] * p[2],
            r[1][0] * p[0] + r[1][1] * p[1] + r[1][2] * p[2],
            r[2][0] * p[0] + r[2][1] * p[1] + r[2][2] * p[2],
        ];
        self.lookup(d)
    }
}

/// Bilinear sample of a `size` x `size` plane at (u, v), pixel centres at integer positions. Edges repeat.
#[inline(always)]
fn sample(plane: &[u8], size: usize, u: f32, v: f32) -> f32 {
    let max = (size - 1) as f32;
    let (u, v) = (u.clamp(0.0, max), v.clamp(0.0, max));
    let (x, y) = (u as usize, v as usize);
    let (x1, y1) = ((x + 1).min(size - 1), (y + 1).min(size - 1));
    let (fx, fy) = (u - x as f32, v - y as f32);
    let p = |xx: usize, yy: usize| plane[yy * size + xx] as f32;
    (p(x, y) * (1.0 - fx) + p(x1, y) * fx) * (1.0 - fy)
        + (p(x, y1) * (1.0 - fx) + p(x1, y1) * fx) * fy
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lens::Lens;
    use crate::math::{axis_rotation, IDENTITY};

    fn x5_like(size: f64) -> LensModel {
        let lens = Lens {
            rotation: IDENTITY,
            cx: size / 2.0,
            cy: size / 2.0,
            radius: size * 0.492,
        };
        LensModel {
            lenses: [
                lens,
                Lens {
                    rotation: axis_rotation('y', 180.0),
                    ..lens
                },
            ],
            coefficients: [0.0, 0.03159, -8.415e-5, 2.201e-6, -1.284e-8],
            half_fov: 97.5,
            level: IDENTITY,
        }
    }

    #[test]
    fn the_lens_table_agrees_with_the_lens_model() {
        // The X5 8K lens stream. The table interpolates the positions; the model is exact.
        let size = 3840;
        let model = x5_like(size as f64);
        let frames = FrameStitcher::new(&model, size, 2 * size).unwrap();
        let (mut worst, mut worst_alpha, mut count) = (0f64, 0f64, 0);
        // A golden-angle spiral gives directions spread over the full sphere.
        for i in 0..20_000 {
            let z = 1.0 - 2.0 * (i as f64 + 0.5) / 20_000.0;
            let a = i as f64 * 2.399_963;
            let r = (1.0 - z * z).sqrt();
            let d = [r * a.cos(), r * a.sin(), z];
            let exact = model.ray(&d);
            let got = frames.lookup([d[0] as f32, d[1] as f32, d[2] as f32]);
            for l in 0..2 {
                // Only where the lens sees the ray, and not in the last 0.2 degrees (one cell).
                if exact.phi[l] < model.half_fov - 0.2 {
                    let du = (got.u[l] as f64 - exact.u[l]).hypot(got.v[l] as f64 - exact.v[l]);
                    worst = worst.max(du);
                    count += 1;
                }
            }
            worst_alpha = worst_alpha.max((got.alpha as f64 - exact.alpha).abs());
        }
        assert!(count > 10_000);
        assert!(worst < 0.1, "largest position error {worst:.4} px");
        assert!(
            worst_alpha < 0.01,
            "largest seam weight error {worst_alpha:.4}"
        );
    }
}
