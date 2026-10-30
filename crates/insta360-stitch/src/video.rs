//! This module stitches the frames of a video one at a time, with a different rotation for each frame.
//!
//! The lens model is slow: each ray needs trigonometry and a polynomial. Thus, the module calculates it only on a
//! grid of the output: one exact ray for each `step` x `step` pixels (approximately 0.375 degree), with the
//! rotation of the frame. Between the grid points, it interpolates the lens positions linearly. The lens positions
//! change slowly over the output, thus the grid adds less than 0.02 pixel at 8K. The chroma samples use the same
//! grid.
//! Insta360 Studio uses a similar design on the GPU: a lens lookup map, and a rotation matrix for each frame.
//!
//! The seam flow (see `seamflow`) moves the lens positions at the grid points near the seam. Studio uses a neural
//! flow for videos; the library uses the DIS flow of the photo export, with the mean of the flows of the frames
//! t - 2 to t + 2 for frame t, as Studio does for its neural flow.
//!
//! The colour fusion (see `fusion`) is calculated for each frame from the frame itself, as Studio does. The stitch
//! changes the colour of each lens near the seam before the blend, in a second pass over the pixels where a gain is
//! not 1.

use crate::error::{invalid, unsupported, Result};
use crate::fusion::{fusion_maps, FusionMaps};
use crate::lens::{blend_weight, LensModel};
use crate::math::{apply, Mat3};
use crate::seamflow::{Flows, SeamFlow, Strips};
use crate::stitch::MAX_VIDEO_WIDTH;
use std::sync::OnceLock;

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

/// The rotations of one frame. A rolling shutter reads the sensor rows one after the other: each row has its
/// own time, thus its own rotation.
#[derive(Debug, Clone, PartialEq)]
pub struct FrameRotation {
    /// The output direction -> camera rotation at the frame time (the centre row of lens 0).
    pub centre: Mat3,
    /// For each lens: the rotations at evenly spaced times from the first (top) to the last (bottom) row of that
    /// lens. Empty: the same rotation for all rows (no rolling shutter correction).
    pub rows: [Vec<Mat3>; 2],
}

impl FrameRotation {
    /// One rotation for all rows.
    pub fn fixed(rotation: Mat3) -> Self {
        FrameRotation {
            centre: rotation,
            rows: [Vec::new(), Vec::new()],
        }
    }
}

/// The rotation at the fraction `f` (0 = top row, 1 = bottom row) of evenly spaced row rotations. The rotations
/// of one frame differ by less than a degree, thus a linear interpolation of the matrices is sufficient.
fn row_rotation(rows: &[Mat3], f: f64) -> Mat3 {
    let x = f * (rows.len() - 1) as f64;
    let i = (x as usize).min(rows.len() - 2);
    let w = x - i as f64;
    std::array::from_fn(|r| {
        std::array::from_fn(|c| rows[i][r][c] * (1.0 - w) + rows[i + 1][r][c] * w)
    })
}

/// The seam flow of one video frame, from [`FrameStitcher::frame_flow`].
#[derive(Debug, Clone)]
pub struct FrameFlow(Flows);

impl FrameFlow {
    /// The mean of the flows of several frames. For frame t, Insta360 uses the frames t - 2 to t + 2.
    /// `flows` must not be empty.
    pub fn mean(flows: &[&FrameFlow]) -> FrameFlow {
        assert!(!flows.is_empty(), "the mean of no flows");
        FrameFlow(Flows::mean(&flows.iter().map(|f| &f.0).collect::<Vec<_>>()))
    }
}

/// The colour fusion of one video frame, from [`FrameStitcher::frame_fusion`].
#[derive(Debug, Clone)]
pub struct FrameFusion(FusionMaps);

/// Stitches the frames of a video. See the module documentation.
pub struct FrameStitcher {
    model: LensModel,
    lens_size: usize,
    width: usize,
    step: usize,
    /// The seam strips, made at the first seam flow. `None` for a model without the Studio blend.
    strips: OnceLock<Option<Strips>>,
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
            strips: OnceLock::new(),
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

    /// The exact lens model at each grid point, for the rotations of one frame.
    /// With a rolling shutter (as Insta360 `FlowstateRender2::backMapPoint`): the rotation at the frame time gives
    /// the row v of each lens. Then the rotation at the time of that row, `t - sweep / 2 + sweep * v / size`, gives
    /// the position in that lens. The seam weight comes from the rotation at the frame time.
    /// With `seam`, the lens positions near the seam move, and the seam weight uses the lens masks at the moved
    /// positions (as for a photo).
    fn grid(&self, rotation: &FrameRotation, seam: Option<&SeamFlow>) -> Vec<Point> {
        let gw = self.grid_cols.len();
        let size = self.lens_size as f64;
        let point = |k: usize| -> Point {
            let ((sp, cp), (st, ct)) = (self.grid_rows[k / gw], self.grid_cols[k % gw]);
            // The same output direction as `LensModel::row`.
            let p = [-sp * st, cp, sp * ct];
            let ray = self.model.ray(&apply(&rotation.centre, &p));
            let mut point = [
                ray.u[0] as f32,
                ray.v[0] as f32,
                ray.u[1] as f32,
                ray.v[1] as f32,
                ray.alpha as f32,
                (self.model.half_fov - ray.phi[0]) as f32,
                (self.model.half_fov - ray.phi[1]) as f32,
            ];
            for (l, rows) in rotation.rows.iter().enumerate() {
                if rows.is_empty() {
                    continue;
                }
                let row = row_rotation(rows, (ray.v[l] / size).clamp(0.0, 1.0));
                let lens = self.model.ray(&apply(&row, &p));
                point[2 * l] = lens.u[l] as f32;
                point[2 * l + 1] = lens.v[l] as f32;
                point[5 + l] = (self.model.half_fov - lens.phi[l]) as f32;
            }
            let d = apply(&rotation.centre, &p);
            if let Some(shift) = seam.and_then(|seam| seam.shift(&d)) {
                for l in 0..2 {
                    point[2 * l] += shift[l].0;
                    point[2 * l + 1] += shift[l].1;
                }
                if let Some(blend) = &self.model.blend {
                    let moved = std::array::from_fn(|l| {
                        (
                            ray.u[l] + f64::from(shift[l].0),
                            ray.v[l] + f64::from(shift[l].1),
                        )
                    });
                    point[4] = blend.weight(&d, moved) as f32;
                }
            }
            point
        };
        let count = gw * self.grid_rows.len();
        #[cfg(feature = "parallel")]
        return (0..count).into_par_iter().map(point).collect();
        #[cfg(not(feature = "parallel"))]
        return (0..count).map(point).collect();
    }

    /// Stitches one frame. `rotation` is the output direction -> camera rotation of the frame
    /// (see [`crate::frame_orientations`]). `src` has `input_frame_bytes`, `out` has `output_frame_bytes`.
    /// For the rolling shutter correction and the seam flow, use [`FrameStitcher::stitch_yuv420_flow`].
    pub fn stitch_yuv420(&self, rotation: &Mat3, src: &[u8], out: &mut [u8]) -> Result<()> {
        self.stitch_yuv420_flow(&FrameRotation::fixed(*rotation), None, src, out)
    }

    /// The seam flow of one input frame (`input_frame_bytes`, see [`FrameStitcher::stitch_yuv420_flow`]).
    /// `None` for a model without the Studio blend. It takes approximately 0.1 s, independent of the size.
    pub fn frame_flow(&self, src: &[u8]) -> Result<Option<FrameFlow>> {
        if src.len() != self.input_frame_bytes() {
            return invalid(format!(
                "a frame must have {} input bytes, not {}",
                self.input_frame_bytes(),
                src.len()
            ));
        }
        let size = self.lens_size;
        let Some(strips) = self
            .strips
            .get_or_init(|| Strips::new(&self.model, size as f64))
        else {
            return Ok(None);
        };
        let luma = [&src[..size * size], &src[size * size..2 * size * size]];
        Ok(Some(FrameFlow(
            strips.flows(|l, u, v| sample(luma[l], size, u, v)),
        )))
    }

    /// The colour fusion of one input frame (`input_frame_bytes`, full range, BT.709, as the X5 records). As the
    /// Studio export ("Chromatic Calibration"), it matches the colours of the two lenses at the seam. Studio
    /// calculates it for each frame. See [`FrameStitcher::stitch_yuv420_full`].
    pub fn frame_fusion(&self, src: &[u8]) -> Result<FrameFusion> {
        if src.len() != self.input_frame_bytes() {
            return invalid(format!(
                "a frame must have {} input bytes, not {}",
                self.input_frame_bytes(),
                src.len()
            ));
        }
        let size = self.lens_size;
        let lenses = Lenses::new(src, size);
        Ok(FrameFusion(fusion_maps(&self.model, size, |l, u, v| {
            lenses
                .rgb(l, u as f32, v as f32)
                .map(|c| c.clamp(0.0, 255.0))
        })))
    }

    /// Stitches one frame with a rotation for each sensor row and the seam `flow` (for example the mean of
    /// [`FrameStitcher::frame_flow`] of the frames t - 2 to t + 2). `None`: no seam flow.
    pub fn stitch_yuv420_flow(
        &self,
        rotation: &FrameRotation,
        flow: Option<&FrameFlow>,
        src: &[u8],
        out: &mut [u8],
    ) -> Result<()> {
        self.stitch_yuv420_full(rotation, flow, None, src, out)
    }

    /// As [`FrameStitcher::stitch_yuv420_flow`], with the colour `fusion` of the frame (from
    /// [`FrameStitcher::frame_fusion`]). `None`: no colour fusion. Near the seam, the stitch changes the colour of
    /// each lens before the blend, as the Studio shader does.
    pub fn stitch_yuv420_full(
        &self,
        rotation: &FrameRotation,
        flow: Option<&FrameFlow>,
        fusion: Option<&FrameFusion>,
        src: &[u8],
        out: &mut [u8],
    ) -> Result<()> {
        if src.len() != self.input_frame_bytes() || out.len() != self.output_frame_bytes() {
            return invalid(format!(
                "a frame must have {} input bytes and {} output bytes, not {} and {}",
                self.input_frame_bytes(),
                self.output_frame_bytes(),
                src.len(),
                out.len()
            ));
        }
        let all = std::iter::once(&rotation.centre).chain(rotation.rows.iter().flatten());
        if all.flatten().flatten().any(|v| !v.is_finite()) {
            return invalid("the frame rotation is not finite");
        }
        if rotation.rows.iter().any(|rows| rows.len() == 1) {
            return invalid("the row rotations of a lens need at least two times");
        }
        let seam = flow.and_then(|flow| {
            let strips = self
                .strips
                .get_or_init(|| Strips::new(&self.model, self.lens_size as f64));
            Some(SeamFlow::from_flows(&self.model, strips.as_ref()?, &flow.0))
        });
        let grid = self.grid(rotation, seam.as_ref());
        let gw = self.grid_cols.len();
        let size = self.lens_size;
        let (luma, chroma) = src.split_at(size * size * 2);
        let (cb, cr) = chroma.split_at(size * size / 2);
        let (w, h) = (self.width, self.height());
        let (out_luma, out_chroma) = out.split_at_mut(w * h);
        let (out_cb, out_cr) = out_chroma.split_at_mut(w * h / 4);

        let line = |y: f32| grid_line(&grid, gw, self.step, y);
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
        if let Some(fusion) = fusion {
            self.fuse(rotation, fusion, &grid, src, (out_luma, out_cb, out_cr));
        }
        Ok(())
    }

    /// The gains of grid point `k` (lens 0, then lens 1, [r, g, b] each) for the rotations of one frame.
    fn grid_gains(&self, rotation: &FrameRotation, fusion: &FrameFusion, k: usize) -> [f32; 6] {
        let ((sp, cp), (st, ct)) = (
            self.grid_rows[k / self.grid_cols.len()],
            self.grid_cols[k % self.grid_cols.len()],
        );
        // The camera direction of the grid point, as in `grid`.
        let d = apply(&rotation.centre, &[-sp * st, cp, sp * ct]);
        let (a, b) = (fusion.0.gain_at(0, &d), fusion.0.gain_at(1, &d));
        [a[0], a[1], a[2], b[0], b[1], b[2]]
    }

    /// The colour fusion: near the seam, where a gain is not 1, it makes the pixels again with the colour of each
    /// lens changed before the blend (c' = (c + 255) * gain - 255 in RGB, as the Studio shader). It uses the same
    /// grid, thus the pixels agree with the first pass where the gain is 1.
    fn fuse(
        &self,
        rotation: &FrameRotation,
        fusion: &FrameFusion,
        grid: &[Point],
        src: &[u8],
        (out_luma, out_cb, out_cr): (&mut [u8], &mut [u8], &mut [u8]),
    ) {
        type Wide = [f32; 13];
        let gw = self.grid_cols.len();
        let wide_point = |k: usize| -> Wide {
            let gains = self.grid_gains(rotation, fusion, k);
            std::array::from_fn(|i| if i < 7 { grid[k][i] } else { gains[i - 7] })
        };
        #[cfg(feature = "parallel")]
        let wide: Vec<Wide> = (0..grid.len()).into_par_iter().map(wide_point).collect();
        #[cfg(not(feature = "parallel"))]
        let wide: Vec<Wide> = (0..grid.len()).map(wide_point).collect();
        // A grid cell (the square between four points) is active if one of its gains is not 1.
        let changed = |p: &Wide| p[7..].iter().any(|g| (g - 1.0).abs() > GAIN_EPSILON);
        let cells = gw - 1;
        let active: Vec<bool> = (0..grid.len() / gw - 1)
            .flat_map(|j| (0..cells).map(move |i| (j, i)))
            .map(|(j, i)| {
                [(j, i), (j, i + 1), (j + 1, i), (j + 1, i + 1)]
                    .iter()
                    .any(|&(j, i)| changed(&wide[j * gw + i]))
            })
            .collect();
        let rows = grid.len() / gw - 1;

        let lenses = Lenses::new(src, self.lens_size);
        // Y, Cb and Cr of the pixel at wide point `p`: the colour of each lens (chroma as in the first pass), changed
        // by its gain, then the blend.
        let pixel = |p: &Wide| -> [f32; 3] {
            let colour = |l: usize| -> [f32; 3] {
                let c = lenses.rgb(l, p[2 * l], p[2 * l + 1]);
                std::array::from_fn(|k| {
                    ((c[k].clamp(0.0, 255.0) + 255.0) * p[7 + 3 * l + k] - 255.0).clamp(0.0, 255.0)
                })
            };
            let weight = blend_weight(p[5] > 0.0, p[6] > 0.0, p[4]);
            let c = if weight >= 1.0 {
                colour(0)
            } else if weight <= 0.0 {
                colour(1)
            } else {
                let (a, b) = (colour(0), colour(1));
                std::array::from_fn(|k| a[k] * weight + b[k] * (1.0 - weight))
            };
            ycc_from_rgb(&c)
        };
        let (w, step) = (self.width, self.step);
        // Two luma rows and one chroma row (4:2:0, left siting) at a time.
        let pair = |cy: usize, (luma_rows, row_b, row_r): (&mut [u8], &mut [u8], &mut [u8])| {
            let (top, bottom) = luma_rows.split_at_mut(w);
            let lines = [2 * cy, 2 * cy + 1].map(|y| grid_line(&wide, gw, step, y as f32));
            let cell_row = |y: usize| (y / step).min(rows - 1);
            let (j0, j1) = (cell_row(2 * cy), cell_row(2 * cy + 1));
            for x in 0..w {
                let i = (x / step).min(cells - 1);
                if !(active[j0 * cells + i] || active[j1 * cells + i]) {
                    continue;
                }
                let f = (x - i * step) as f32 / step as f32;
                let at = |line: &[Wide]| -> Wide {
                    std::array::from_fn(|k| line[i][k] + (line[i + 1][k] - line[i][k]) * f)
                };
                let (c0, c1) = (pixel(&at(&lines[0])), pixel(&at(&lines[1])));
                top[x] = byte(c0[0]);
                bottom[x] = byte(c1[0]);
                if x % 2 == 0 {
                    row_b[x / 2] = byte((c0[1] + c1[1]) / 2.0);
                    row_r[x / 2] = byte((c0[2] + c1[2]) / 2.0);
                }
            }
        };
        #[cfg(feature = "parallel")]
        out_luma
            .par_chunks_mut(2 * w)
            .zip(out_cb.par_chunks_mut(w / 2))
            .zip(out_cr.par_chunks_mut(w / 2))
            .enumerate()
            .for_each(|(cy, ((l, b), r))| pair(cy, (l, b, r)));
        #[cfg(not(feature = "parallel"))]
        for (cy, ((l, b), r)) in out_luma
            .chunks_mut(2 * w)
            .zip(out_cb.chunks_mut(w / 2))
            .zip(out_cr.chunks_mut(w / 2))
            .enumerate()
        {
            pair(cy, (l, b, r));
        }
    }
}

/// The grid (`columns` points in a row) at luma row position `y` (it can be between two rows): one point for each
/// grid column.
fn grid_line<const N: usize>(
    grid: &[[f32; N]],
    columns: usize,
    step: usize,
    y: f32,
) -> Vec<[f32; N]> {
    let j = ((y / step as f32) as usize).min(grid.len() / columns - 2);
    let f = y / step as f32 - j as f32;
    let (top, bottom) = (
        &grid[j * columns..(j + 1) * columns],
        &grid[(j + 1) * columns..(j + 2) * columns],
    );
    top.iter()
        .zip(bottom)
        .map(|(a, b)| std::array::from_fn(|i| a[i] + (b[i] - a[i]) * f))
        .collect()
}

/// Walks each `stride`-th luma column x in `0..len` of one grid line (a point for each grid column, `step` pixels
/// apart) and gives `pixel(point, x)` the interpolated point at x.
#[inline(always)]
fn fill<const N: usize>(
    line: &[[f32; N]],
    step: usize,
    len: usize,
    stride: usize,
    mut pixel: impl FnMut(&[f32; N], usize),
) {
    for (k, pair) in line.windows(2).enumerate() {
        let start = k * step;
        if start >= len {
            break;
        }
        let (a, b) = (&pair[0], &pair[1]);
        let d: [f32; N] = std::array::from_fn(|i| (b[i] - a[i]) / step as f32);
        let first = start.next_multiple_of(stride);
        let mut p: [f32; N] = std::array::from_fn(|i| a[i] + d[i] * (first - start) as f32);
        for x in (first..(start + step).min(len)).step_by(stride) {
            pixel(&p, x);
            for i in 0..N {
                p[i] += d[i] * stride as f32;
            }
        }
    }
}

/// The planes of the two lenses of a yuv420p frame (lens 0 on top).
struct Lenses<'a> {
    size: usize,
    planes: [(&'a [u8], &'a [u8], &'a [u8]); 2],
}

impl<'a> Lenses<'a> {
    fn new(src: &'a [u8], size: usize) -> Self {
        let half = size / 2;
        let (luma, chroma) = src.split_at(size * size * 2);
        let (cb, cr) = chroma.split_at(size * size / 2);
        let lens = |l: usize| {
            (
                &luma[l * size * size..(l + 1) * size * size],
                &cb[l * half * half..(l + 1) * half * half],
                &cr[l * half * half..(l + 1) * half * half],
            )
        };
        Lenses {
            size,
            planes: [lens(0), lens(1)],
        }
    }

    /// The colour of lens `l` at the lens position (u, v), with the chroma siting of the stitch (not clamped).
    fn rgb(&self, l: usize, u: f32, v: f32) -> [f32; 3] {
        let (y, cb, cr) = self.planes[l];
        let half = self.size / 2;
        rgb_from_ycc(
            sample(y, self.size, u, v),
            sample(cb, half, u * 0.5, (v - 0.5) * 0.5),
            sample(cr, half, u * 0.5, (v - 0.5) * 0.5),
        )
    }
}

/// The colour of a pixel from its Y, Cb and Cr (0 to 255, full range, BT.709: the X5 records this).
fn rgb_from_ycc(y: f32, cb: f32, cr: f32) -> [f32; 3] {
    let (cb, cr) = (cb - 128.0, cr - 128.0);
    [
        y + 1.5748 * cr,
        y - 0.18732 * cb - 0.46812 * cr,
        y + 1.8556 * cb,
    ]
}

fn ycc_from_rgb(c: &[f32; 3]) -> [f32; 3] {
    let y = 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
    [y, (c[2] - y) / 1.8556 + 128.0, (c[0] - y) / 1.5748 + 128.0]
}

fn byte(v: f32) -> u8 {
    (v + 0.5).clamp(0.0, 255.0) as u8
}

/// A gain that is not 1 by more than this changes a pixel by less than one level at the most.
const GAIN_EPSILON: f32 = 1e-3;

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
            mei: None,
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
            blend: None,
        }
    }

    #[test]
    fn a_pixel_comes_only_from_the_lenses_that_see_it() {
        // Lens 0 is black, lens 1 is 200. The seam weight of lens 0 is 0.7.
        let (black, grey) = (vec![0u8; 16], vec![200u8; 16]);
        let lenses = [&black[..], &grey[..]];
        let point = |fov0: f32, fov1: f32| -> Point { [1.0, 1.0, 1.0, 1.0, 0.7, fov0, fov1] };
        assert_eq!(blend(&point(1.0, 1.0), lenses, 4), 60);
        // Outside the field of view of a lens (negative margin), the other lens gives the full pixel.
        assert_eq!(blend(&point(-1.0, 1.0), lenses, 4), 200);
        assert_eq!(blend(&point(1.0, -1.0), lenses, 4), 0);
    }

    #[test]
    fn the_grid_agrees_with_the_lens_model_between_its_points() {
        // The X5 8K lens stream and a tilted frame rotation. The grid is exact at its points and linear between.
        let size = 3840;
        let model = x5_like(size as f64);
        let frames = FrameStitcher::new(&model, size, 2 * size).unwrap();
        let r = rotate_matrix(12.0, -25.0, 40.0);
        let grid = frames.grid(&FrameRotation::fixed(r), None);
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

    #[test]
    fn with_a_rolling_shutter_each_lens_uses_the_rotation_of_its_row() {
        let size = 960;
        let model = x5_like(size as f64);
        let frames = FrameStitcher::new(&model, size, 2 * size).unwrap();
        let centre = rotate_matrix(12.0, -25.0, 40.0);
        // The same rotation for each row: the same grid as without a rolling shutter.
        let same = FrameRotation {
            centre,
            rows: [vec![centre; 3], vec![centre; 3]],
        };
        assert_eq!(
            frames.grid(&same, None),
            frames.grid(&FrameRotation::fixed(centre), None)
        );
        // The top row 2 degrees before, the bottom row 2 degrees after (lens 1: the other direction).
        let (top, bottom) = (
            rotate_matrix(10.0, -25.0, 40.0),
            rotate_matrix(14.0, -25.0, 40.0),
        );
        let rolling = FrameRotation {
            centre,
            rows: [vec![top, centre, bottom], vec![bottom, centre, top]],
        };
        let grid = frames.grid(&rolling, None);
        let (gw, mut checked) = (frames.grid_cols.len(), 0);
        for (k, got) in grid.iter().enumerate().step_by(7) {
            let ((sp, cp), (st, ct)) = (frames.grid_rows[k / gw], frames.grid_cols[k % gw]);
            let p = [-sp * st, cp, sp * ct];
            let at_centre = model.ray(&apply(&centre, &p));
            for l in 0..2 {
                // The row of the centre rotation gives the time; top = first rotation, bottom = last.
                let f = (at_centre.v[l] / size as f64).clamp(0.0, 1.0);
                let rows = &rolling.rows[l];
                let (a, b) = if f < 0.5 {
                    (rows[0], rows[1])
                } else {
                    (rows[1], rows[2])
                };
                let w = if f < 0.5 { 2.0 * f } else { 2.0 * f - 1.0 };
                let r: Mat3 = std::array::from_fn(|i| {
                    std::array::from_fn(|j| a[i][j] * (1.0 - w) + b[i][j] * w)
                });
                let want = model.ray(&apply(&r, &p));
                assert!(
                    (got[2 * l] as f64 - want.u[l]).abs() < 1e-3,
                    "u of lens {l}"
                );
                assert!(
                    (got[2 * l + 1] as f64 - want.v[l]).abs() < 1e-3,
                    "v of lens {l}"
                );
                checked += 1;
            }
            // The seam weight comes from the rotation at the frame time.
            assert!((got[4] as f64 - at_centre.alpha).abs() < 1e-6);
        }
        assert!(checked > 1000);
        // A grid point at a top row of lens 0 moves with the top rotation: away from the fixed grid.
        let fixed = frames.grid(&FrameRotation::fixed(centre), None);
        assert!(grid
            .iter()
            .zip(&fixed)
            .any(|(a, b)| (a[0] - b[0]).abs() > 1.0));
    }

    /// A frame with a flat colour in each lens: Y, Cb and Cr of lens 0, then of lens 1.
    fn flat_frame(size: usize, ycc: [[u8; 3]; 2]) -> Vec<u8> {
        let mut frame = Vec::new();
        for c in 0..3 {
            let side = if c == 0 { size } else { size / 2 };
            for lens in ycc {
                frame.extend(std::iter::repeat_n(lens[c], side * side));
            }
        }
        frame
    }

    #[test]
    fn the_fusion_changes_the_colour_of_a_lens_before_the_blend() {
        let (size, width) = (64, 128);
        let frames = FrameStitcher::new(&x5_like(size as f64), size, width).unwrap();
        let (c0, c1) = ([120u8, 110, 150], [90u8, 140, 100]);
        let frame = flat_frame(size, [c0, c1]);
        let mut plain = vec![0u8; frames.output_frame_bytes()];
        let rotation = FrameRotation::fixed(crate::math::IDENTITY);
        frames
            .stitch_yuv420_full(&rotation, None, None, &frame, &mut plain)
            .unwrap();
        // Lens 0 gets a gain of 1.1 (red), 1.0 (green) and 0.9 (blue). Lens 1 does not change.
        let mut maps = FusionMaps::identity();
        maps.gain[0].iter_mut().for_each(|g| *g = [1.1, 1.0, 0.9]);
        let mut fused = vec![0u8; frames.output_frame_bytes()];
        frames
            .stitch_yuv420_full(
                &rotation,
                None,
                Some(&FrameFusion(maps)),
                &frame,
                &mut fused,
            )
            .unwrap();
        // The expected colour of lens 0 after the gain, with the same maths as the Studio shader.
        let rgb = rgb_from_ycc(f32::from(c0[0]), f32::from(c0[1]), f32::from(c0[2]));
        let gain = [1.1, 1.0, 0.9];
        let changed: [f32; 3] = std::array::from_fn(|k| {
            ((rgb[k].clamp(0.0, 255.0) + 255.0) * gain[k] - 255.0).clamp(0.0, 255.0)
        });
        let want = ycc_from_rgb(&changed).map(byte);
        assert_ne!(want[0], c0[0], "the test needs a visible change");
        let (mut only0, mut only1, mut mixed) = (0, 0, 0);
        for (&p, &f) in plain.iter().zip(&fused).take(width * width / 2) {
            if p == c0[0] {
                only0 += 1;
                assert!(
                    f.abs_diff(want[0]) <= 1,
                    "a pixel of lens 0 has the new luma"
                );
            } else if p == c1[0] {
                only1 += 1;
                // A pixel next to the seam has a very small share of lens 0.
                assert!(f.abs_diff(c1[0]) <= 1, "a pixel of lens 1 does not change");
            } else {
                mixed += 1;
                assert!(
                    f >= want[0].min(c1[0]) && f <= want[0].max(c1[0]),
                    "a blend stays between"
                );
            }
        }
        assert!(
            only0 > 1000 && only1 > 1000 && mixed > 0,
            "{only0} {only1} {mixed}"
        );
        // The chroma of the pixels that come only from lens 0 (two luma rows for each chroma row).
        let (cb_at, cr_at) = (width * width / 2, width * width / 2 + width * width / 8);
        let mut chroma_checked = 0;
        for cy in 0..width / 4 {
            for cx in 0..width / 2 {
                let (x, i) = (2 * cx, cy * width / 2 + cx);
                let rows = [2 * cy, 2 * cy + 1].map(|y| plain[y * width + x]);
                if rows == [c0[0]; 2] && plain[cb_at + i] == c0[1] && plain[cr_at + i] == c0[2] {
                    chroma_checked += 1;
                    assert!(fused[cb_at + i].abs_diff(want[1]) <= 1, "Cb of lens 0");
                    assert!(fused[cr_at + i].abs_diff(want[2]) <= 1, "Cr of lens 0");
                }
            }
        }
        assert!(chroma_checked > 100, "{chroma_checked}");
    }

    #[test]
    fn the_gains_of_a_grid_point_are_those_of_its_direction() {
        // A step of 1: grid point (j, i) is at the centre of output pixel (i, j), as `output_direction`.
        let (size, width) = (64, 128);
        let frames = FrameStitcher::new(&x5_like(size as f64), size, width).unwrap();
        let rotation = FrameRotation::fixed(rotate_matrix(12.0, -25.0, 40.0));
        let mut maps = FusionMaps::identity();
        for (l, gain) in maps.gain.iter_mut().enumerate() {
            for (n, g) in gain.iter_mut().enumerate() {
                *g = [
                    1.0 + (n % 97) as f32 * 0.002,
                    1.0 + (n % 53 + l) as f32 * 0.003,
                    1.0,
                ];
            }
        }
        let fusion = FrameFusion(maps);
        let gw = frames.grid_cols.len();
        for (x, y) in [(3, 5), (40, 20), (64, 32), (100, 60), (127, 63)] {
            let d = apply(
                &rotation.centre,
                &crate::lens::output_direction(x, y, width, width / 2),
            );
            let got = frames.grid_gains(&rotation, &fusion, y * gw + x);
            let (a, b) = (fusion.0.gain_at(0, &d), fusion.0.gain_at(1, &d));
            let want = [a[0], a[1], a[2], b[0], b[1], b[2]];
            for k in 0..6 {
                assert!((got[k] - want[k]).abs() < 1e-5, "({x}, {y}) gain {k}");
            }
        }
        // The maps are not flat, thus a wrong direction would give other gains.
        let flat = frames.grid_gains(&rotation, &fusion, 0)
            == frames.grid_gains(&rotation, &fusion, gw * 30 + 70);
        assert!(!flat);
    }

    #[test]
    fn a_fusion_of_gain_one_does_not_change_the_frame() {
        let (size, width) = (64, 128);
        let frames = FrameStitcher::new(&x5_like(size as f64), size, width).unwrap();
        let frame = flat_frame(size, [[120, 110, 150], [90, 140, 100]]);
        let rotation = FrameRotation::fixed(rotate_matrix(5.0, -10.0, 20.0));
        let (mut plain, mut fused) = (
            vec![0u8; frames.output_frame_bytes()],
            vec![0u8; frames.output_frame_bytes()],
        );
        frames
            .stitch_yuv420_full(&rotation, None, None, &frame, &mut plain)
            .unwrap();
        let fusion = FrameFusion(FusionMaps::identity());
        frames
            .stitch_yuv420_full(&rotation, None, Some(&fusion), &frame, &mut fused)
            .unwrap();
        assert_eq!(plain, fused);
    }
}
