//! This module stitches the frames of a video one at a time, with a different rotation for each frame.
//!
//! The lens model is slow: each ray needs trigonometry and a polynomial. Thus, the module calculates it only on a
//! grid of the output: one exact ray for each `step` x `step` pixels (approximately 0.375 degree), with the
//! rotation of the frame. Between the grid points, it interpolates the lens positions linearly. The lens positions
//! change slowly over the output, thus the grid adds less than 0.02 pixel at 8K. The chroma samples use the same
//! grid.
//! Insta360 Studio uses a similar design on the GPU: a lens lookup map, and a rotation matrix for each frame.

use crate::error::{invalid, unsupported, Result};
use crate::lens::{blend_weight, LensModel};
use crate::math::Mat3;
use crate::stitch::MAX_VIDEO_WIDTH;

#[cfg(feature = "parallel")]
use rayon::prelude::*;

/// The output pixels between two grid points (one direction) for an output of `width`: 8 at 7680, 1 (exact) below
/// 1920. At 7680, 8 gives a position error of less than 0.02 pixel and needs 1/64 of the rays of an exact stitch.
fn grid_step(width: usize) -> usize {
    (width / 960).max(1)
}

/// The lens model at one output position: u0, v0, u1, v1 (lens pixels), the seam weight of lens 0, and how far
/// inside the field of view of each lens the ray is (degrees, positive inside).
type Point = [f32; 7];

/// Stitches the frames of a video. See the module documentation.
pub struct FrameStitcher {
    model: LensModel,
    lens_size: usize,
    width: usize,
    step: usize,
    /// sin and cos of the polar angle of each grid row, and of the azimuth of each grid column.
    /// Grid point (i, j) is at the centre of output pixel (i * step, j * step).
    grid_rows: Vec<(f64, f64)>,
    grid_cols: Vec<(f64, f64)>,
}

impl FrameStitcher {
    /// Makes a stitcher for lens streams of `lens_size` x `lens_size` and an output of `width` x `width / 2`.
    /// Make `model` for this lens size and with the identity as level rotation: each frame gives its rotation.
    pub fn new(model: &LensModel, lens_size: usize, width: usize) -> Result<Self> {
        if model.level != crate::math::IDENTITY {
            // A level rotation in the model and the frame rotation would turn the image two times.
            return invalid(
                "make the model of a frame stitcher with the identity as level rotation",
            );
        }
        if lens_size < 8 || !lens_size.is_multiple_of(2) || lens_size > MAX_VIDEO_WIDTH {
            return unsupported(format!("unsupported lens stream size {lens_size}"));
        }
        if width == 0 || !width.is_multiple_of(4) || width > MAX_VIDEO_WIDTH {
            return invalid(format!(
                "the output width must be a multiple of 4 and at most {MAX_VIDEO_WIDTH}, not {width}"
            ));
        }
        let height = width / 2;
        // One more grid point after the last pixel, thus each pixel has grid points on its two sides.
        let angle = |pixel: usize, turn: f64, size: usize| {
            (turn * std::f64::consts::PI * (pixel as f64 + 0.5) / size as f64).sin_cos()
        };
        let step = grid_step(width);
        Ok(FrameStitcher {
            model: model.clone(),
            lens_size,
            width,
            step,
            grid_rows: (0..=height.div_ceil(step))
                .map(|j| angle(j * step, 1.0, height))
                .collect(),
            grid_cols: (0..=width.div_ceil(step))
                .map(|i| angle(i * step, 2.0, width))
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

    /// The exact lens model at each grid point, for the output direction -> camera rotation `r`.
    fn grid(&self, r: &Mat3) -> Vec<Point> {
        let gw = self.grid_cols.len();
        let point = |k: usize| -> Point {
            let ((sp, cp), (st, ct)) = (self.grid_rows[k / gw], self.grid_cols[k % gw]);
            // The same output direction as `LensModel::row`.
            let p = [-sp * st, cp, sp * ct];
            let d: [f64; 3] =
                std::array::from_fn(|i| r[i][0] * p[0] + r[i][1] * p[1] + r[i][2] * p[2]);
            let ray = self.model.ray(&d);
            [
                ray.u[0] as f32,
                ray.v[0] as f32,
                ray.u[1] as f32,
                ray.v[1] as f32,
                ray.alpha as f32,
                (self.model.half_fov - ray.phi[0]) as f32,
                (self.model.half_fov - ray.phi[1]) as f32,
            ]
        };
        let count = gw * self.grid_rows.len();
        #[cfg(feature = "parallel")]
        return (0..count).into_par_iter().map(point).collect();
        #[cfg(not(feature = "parallel"))]
        return (0..count).map(point).collect();
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
        let grid = self.grid(rotation);
        let gw = self.grid_cols.len();
        let size = self.lens_size;
        let (luma, chroma) = src.split_at(size * size * 2);
        let (cb, cr) = chroma.split_at(size * size / 2);
        let (w, h) = (self.width, self.height());
        let (out_luma, out_chroma) = out.split_at_mut(w * h);
        let (out_cb, out_cr) = out_chroma.split_at_mut(w * h / 4);

        // The grid at luma row position `y` (it can be between two rows): one point for each grid column.
        let line = |y: f32| -> Vec<Point> {
            let j = ((y / self.step as f32) as usize).min(self.grid_rows.len() - 2);
            let f = y / self.step as f32 - j as f32;
            let (top, bottom) = (
                &grid[j * gw..(j + 1) * gw],
                &grid[(j + 1) * gw..(j + 2) * gw],
            );
            top.iter()
                .zip(bottom)
                .map(|(a, b)| std::array::from_fn(|i| a[i] + (b[i] - a[i]) * f))
                .collect()
        };
        let luma_lens = [&luma[..size * size], &luma[size * size..]];
        let luma_row = |y: usize, row: &mut [u8]| {
            fill(&line(y as f32), self.step, row.len(), 1, |p, x| {
                row[x] = blend(p, luma_lens, size);
            });
        };
        let half = size / 2;
        let cb_lens = [&cb[..half * half], &cb[half * half..]];
        let cr_lens = [&cr[..half * half], &cr[half * half..]];
        // Chroma (x, y) is at luma column 2x and between luma rows 2y and 2y + 1 (4:2:0, left siting).
        let chroma_row = |y: usize, (row_b, row_r): (&mut [u8], &mut [u8])| {
            // The luma grid line at this row; chroma x is luma column 2x.
            fill(
                &line(2.0 * y as f32 + 0.5),
                self.step,
                2 * row_b.len(),
                2,
                |p, luma_x| {
                    let x = luma_x / 2;
                    // Luma pixel centre (u, v) is at chroma (u / 2, (v - 0.5) / 2).
                    let c = [
                        p[0] * 0.5,
                        (p[1] - 0.5) * 0.5,
                        p[2] * 0.5,
                        (p[3] - 0.5) * 0.5,
                        p[4],
                        p[5],
                        p[6],
                    ];
                    row_b[x] = blend(&c, cb_lens, half);
                    row_r[x] = blend(&c, cr_lens, half);
                },
            );
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
}

/// Walks each `stride`-th luma column x in `0..len` of one grid line (a point for each grid column, `step` pixels
/// apart) and gives `pixel(point, x)` the interpolated point at x.
#[inline(always)]
fn fill(
    line: &[Point],
    step: usize,
    len: usize,
    stride: usize,
    mut pixel: impl FnMut(&Point, usize),
) {
    for (k, pair) in line.windows(2).enumerate() {
        let start = k * step;
        if start >= len {
            break;
        }
        let (a, b) = (&pair[0], &pair[1]);
        let d: Point = std::array::from_fn(|i| (b[i] - a[i]) / step as f32);
        let first = start.next_multiple_of(stride);
        let mut p: Point = std::array::from_fn(|i| a[i] + d[i] * (first - start) as f32);
        for x in (first..(start + step).min(len)).step_by(stride) {
            pixel(&p, x);
            for i in 0..7 {
                p[i] += d[i] * stride as f32;
            }
        }
    }
}

/// The pixel at lens model point `p`. Outside the seam, one lens has the weight 0: that lens is not sampled.
#[inline(always)]
fn blend(p: &Point, lens: [&[u8]; 2], size: usize) -> u8 {
    let weight = blend_weight(p[5] > 0.0, p[6] > 0.0, p[4]);
    let value = if weight >= 1.0 {
        sample(lens[0], size, p[0], p[1])
    } else if weight <= 0.0 {
        sample(lens[1], size, p[2], p[3])
    } else {
        sample(lens[0], size, p[0], p[1]) * weight
            + sample(lens[1], size, p[2], p[3]) * (1.0 - weight)
    };
    (value + 0.5) as u8
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
    use crate::math::{axis_rotation, rotate_matrix, IDENTITY};

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
    fn the_grid_agrees_with_the_lens_model_between_its_points() {
        // The X5 8K lens stream and a tilted frame rotation. The grid is exact at its points and linear between.
        let size = 3840;
        let model = x5_like(size as f64);
        let frames = FrameStitcher::new(&model, size, 2 * size).unwrap();
        let r = rotate_matrix(12.0, -25.0, 40.0);
        let grid = frames.grid(&r);
        let (gw, w, h) = (frames.grid_cols.len(), 2 * size, size);
        let (mut worst, mut worst_alpha, mut count) = (0f64, 0f64, 0);
        // Every 13th pixel: all positions between the grid points.
        for y in (0..h).step_by(13) {
            for x in (0..w).step_by(13) {
                let step = frames.step;
                let (j, i) = (y / step, x / step);
                let (fy, fx) = (
                    (y % step) as f32 / step as f32,
                    (x % step) as f32 / step as f32,
                );
                let at = |i: usize, j: usize| &grid[j * gw + i];
                let got: Point = std::array::from_fn(|k| {
                    let top = at(i, j)[k] + (at(i + 1, j)[k] - at(i, j)[k]) * fx;
                    let bottom = at(i, j + 1)[k] + (at(i + 1, j + 1)[k] - at(i, j + 1)[k]) * fx;
                    top + (bottom - top) * fy
                });
                let (sp, cp) = (std::f64::consts::PI * (y as f64 + 0.5) / h as f64).sin_cos();
                let (st, ct) = (2.0 * std::f64::consts::PI * (x as f64 + 0.5) / w as f64).sin_cos();
                let p = [-sp * st, cp, sp * ct];
                let d: [f64; 3] =
                    std::array::from_fn(|k| r[k][0] * p[0] + r[k][1] * p[1] + r[k][2] * p[2]);
                let exact = model.ray(&d);
                for l in 0..2 {
                    // Only where the lens sees the ray.
                    if exact.phi[l] < model.half_fov {
                        let du = (got[2 * l] as f64 - exact.u[l])
                            .hypot(got[2 * l + 1] as f64 - exact.v[l]);
                        worst = worst.max(du);
                        count += 1;
                    }
                }
                worst_alpha = worst_alpha.max((got[4] as f64 - exact.alpha).abs());
            }
        }
        assert!(count > 100_000);
        // The cube-map table of version 0.0.1 had 0.086 pixel.
        assert!(worst < 0.03, "largest position error {worst:.4} px");
        // The seam weight has corners (the ends of the blend). Between two grid points, the line cuts a corner by
        // up to 0.018. At the seam, the two lenses show almost the same value, thus this has no visible effect.
        assert!(
            worst_alpha < 0.02,
            "largest seam weight error {worst_alpha:.4}"
        );
    }
}
