//! A temporal block denoise for stitched video frames (planar YUV 4:2:0, 8 bit).
//!
//! The filter averages each frame with the frames before and after it ([`RADIUS`] on each side). It is a motion
//! compensated average with weights that depend on how similar the pixels are, as the block denoise of
//! Insta360 Studio (see the issue "Block denoise" of this project). The steps for one frame:
//!
//! 1. **Block flow.** For each block of 16 x 16 pixels and each other frame of the window, a search finds the
//!    shift and the sum of absolute differences (SAD) of the best match. The search has three levels (1/4, 1/2 and
//!    the full size), with integer shifts. It is our own search, not a copy of an existing one.
//! 2. **Stack.** Each pixel is a weighted mean of the frame itself and of the shifted pixels of the other frames.
//!    The weight of a pixel `v` for a pixel `c` is the smaller of two numbers: a table value that is 256 for
//!    `|v - c| < t`, falls to 0 at `3t`, and a limit of the block. The limit is `256 (C^2 - S^2) / (C^2 + S^2)` for a
//!    block SAD `S` below the cap `C`, and 0 above. The cap is the SAD that a block has when its mean absolute
//!    difference is `t`, and it is a little higher for the nearest frames.
//! 3. **Limit.** The result stays within `limit` levels of the original pixel.
//!
//! The chroma planes use the same shifts and limits with blocks of 8 x 8 pixels.
//!
//! The Studio filter takes `t` and `limit` from a table for the ISO of the frame. The library has no such table and
//! does not read the ISO. It uses one setting ([`KNEE`] and [`LIMIT`]) that we tuned on five clips of the X5, so that the
//! noise in flat areas is near the noise of the Studio export. Clips with a high ISO can need a higher strength.

use std::collections::VecDeque;

#[cfg(feature = "parallel")]
use rayon::prelude::*;

use crate::error::{invalid, Result};

/// The number of frames on each side of a frame in its average.
pub const RADIUS: usize = 3;
/// The size of a block of the luma plane.
const BLOCK: usize = 16;
/// The weight of the frame itself, and the weight of a pixel that is equal to it.
const FULL: i32 = 256;
/// The cap of the nearest frames is this factor higher than the cap of the farthest frame.
const NEAR_CAP: f64 = 1.25;
/// The cap of a chroma block is this factor higher than the cap of the luma block.
const CHROMA_CAP: f64 = 2.0;
/// The chroma limit is this factor of the luma limit.
const CHROMA_LIMIT: f32 = 0.5;
/// The knee `t` of the weight table (levels): a pixel of another frame that differs less has the full weight.
pub const KNEE: f32 = 9.0;
/// The limit of the change of a pixel (levels).
pub const LIMIT: i32 = 4;
/// The search range at the coarse level (a quarter of the frame size), in pixels of that level.
const COARSE_RANGE: i32 = 4;
/// A zero shift wins against a better shift when its SAD is less than this value higher.
const ZERO_BIAS: u32 = 16;

/// A plane of 8 bit samples.
#[derive(Clone, Copy)]
struct Plane<'a> {
    data: &'a [u8],
    width: usize,
    height: usize,
}

impl Plane<'_> {
    /// The sample at (x, y). A position outside the plane gives the nearest sample.
    fn at(&self, x: i32, y: i32) -> u8 {
        let x = x.clamp(0, self.width as i32 - 1) as usize;
        let y = y.clamp(0, self.height as i32 - 1) as usize;
        self.data[y * self.width + x]
    }
}

/// The SAD of the `size` x `size` block of `a` at (x0, y0) and the block of `b` that is moved by (dx, dy).
/// A block at the edge of `a` has fewer pixels.
fn sad(a: Plane, b: Plane, x0: usize, y0: usize, size: usize, dx: i32, dy: i32) -> u32 {
    let (x1, y1) = ((x0 + size).min(a.width), (y0 + size).min(a.height));
    let inside = x0 as i32 + dx >= 0
        && y0 as i32 + dy >= 0
        && x1 as i32 + dx <= b.width as i32
        && y1 as i32 + dy <= b.height as i32;
    let mut sum = 0u32;
    if inside {
        let bx = (x0 as i32 + dx) as usize;
        for y in y0..y1 {
            let row_a = &a.data[y * a.width + x0..y * a.width + x1];
            let by = (y as i32 + dy) as usize;
            let row_b = &b.data[by * b.width + bx..by * b.width + bx + (x1 - x0)];
            sum += row_a
                .iter()
                .zip(row_b)
                .map(|(p, q)| u32::from(p.abs_diff(*q)))
                .sum::<u32>();
        }
    } else {
        for y in y0..y1 {
            for x in x0..x1 {
                sum +=
                    u32::from(a.data[y * a.width + x].abs_diff(b.at(x as i32 + dx, y as i32 + dy)));
            }
        }
    }
    sum
}

/// The plane at half the size (the mean of 2 x 2 samples). The size is rounded down.
fn halve(src: Plane) -> (Vec<u8>, usize, usize) {
    let (w, h) = ((src.width / 2).max(1), (src.height / 2).max(1));
    let mut out = vec![0u8; w * h];
    for y in 0..h {
        for x in 0..w {
            let (x2, y2) = (x as i32 * 2, y as i32 * 2);
            let sum = u32::from(src.at(x2, y2))
                + u32::from(src.at(x2 + 1, y2))
                + u32::from(src.at(x2, y2 + 1))
                + u32::from(src.at(x2 + 1, y2 + 1));
            out[y * w + x] = ((sum + 2) / 4) as u8;
        }
    }
    (out, w, h)
}

/// One frame with the planes that the filter needs.
struct Frame {
    size: (usize, usize),
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
    half: Vec<u8>,
    half_size: (usize, usize),
    quarter: Vec<u8>,
    quarter_size: (usize, usize),
}

impl Frame {
    fn new(bytes: &[u8], width: usize, height: usize) -> Frame {
        let luma = width * height;
        let chroma = luma / 4;
        let y = bytes[..luma].to_vec();
        let plane = Plane {
            data: &y,
            width,
            height,
        };
        let (half, hw, hh) = halve(plane);
        let (quarter, qw, qh) = halve(Plane {
            data: &half,
            width: hw,
            height: hh,
        });
        Frame {
            size: (width, height),
            u: bytes[luma..luma + chroma].to_vec(),
            v: bytes[luma + chroma..luma + 2 * chroma].to_vec(),
            half,
            half_size: (hw, hh),
            quarter,
            quarter_size: (qw, qh),
            y,
        }
    }
}

impl Frame {
    fn full(&self) -> Plane<'_> {
        Plane {
            data: &self.y,
            width: self.size.0,
            height: self.size.1,
        }
    }

    fn half(&self) -> Plane<'_> {
        Plane {
            data: &self.half,
            width: self.half_size.0,
            height: self.half_size.1,
        }
    }

    fn quarter(&self) -> Plane<'_> {
        Plane {
            data: &self.quarter,
            width: self.quarter_size.0,
            height: self.quarter_size.1,
        }
    }
}

/// The shift of one block (pixels of the full frame) and its SAD (levels of the 16 x 16 block).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Motion {
    dx: i32,
    dy: i32,
    sad: u32,
}

/// The shifts of all blocks of `other` against `centre`.
fn flow(centre: &Frame, other: &Frame) -> Vec<Motion> {
    let (cols, rows) = (centre.size.0.div_ceil(BLOCK), centre.size.1.div_ceil(BLOCK));
    // Level 1: the exhaustive search at a quarter of the size.
    let coarse_row = |by: usize| -> Vec<(i32, i32)> {
        (0..cols)
            .map(|bx| {
                let mut best = (u32::MAX, 0, 0);
                for dy in -COARSE_RANGE..=COARSE_RANGE {
                    for dx in -COARSE_RANGE..=COARSE_RANGE {
                        let cost =
                            sad(centre.quarter(), other.quarter(), bx * 4, by * 4, 4, dx, dy)
                                + (dx.unsigned_abs() + dy.unsigned_abs());
                        if cost < best.0 {
                            best = (cost, dx, dy);
                        }
                    }
                }
                (best.1, best.2)
            })
            .collect()
    };
    #[cfg(feature = "parallel")]
    let coarse: Vec<Vec<(i32, i32)>> = (0..rows).into_par_iter().map(coarse_row).collect();
    #[cfg(not(feature = "parallel"))]
    let coarse: Vec<Vec<(i32, i32)>> = (0..rows).map(coarse_row).collect();
    let coarse_at = |bx: i32, by: i32| {
        coarse[by.clamp(0, rows as i32 - 1) as usize][bx.clamp(0, cols as i32 - 1) as usize]
    };
    // Levels 2 and 3: a local search around the vectors of the block and of its neighbours.
    let fine_row = |by: usize| -> Vec<Motion> {
        (0..cols)
            .map(|bx| {
                let (own_x, own_y) = coarse_at(bx as i32, by as i32);
                let cost_half = |dx: i32, dy: i32| {
                    sad(centre.half(), other.half(), bx * 8, by * 8, 8, dx, dy)
                        + 4 * ((dx - 2 * own_x).unsigned_abs() + (dy - 2 * own_y).unsigned_abs())
                };
                let mut best = (u32::MAX, 0, 0);
                let (bxi, byi) = (bx as i32, by as i32);
                for (nx, ny) in [(0, 0), (-1, 0), (1, 0), (0, -1), (0, 1)] {
                    let (cx, cy) = coarse_at(bxi + nx, byi + ny);
                    let cost = cost_half(2 * cx, 2 * cy);
                    if cost < best.0 {
                        best = (cost, 2 * cx, 2 * cy);
                    }
                }
                for _ in 0..2 {
                    let centre_of_step = (best.1, best.2);
                    for (sx, sy) in [
                        (-1, 0),
                        (1, 0),
                        (0, -1),
                        (0, 1),
                        (-1, -1),
                        (1, -1),
                        (-1, 1),
                        (1, 1),
                    ] {
                        let (dx, dy) = (centre_of_step.0 + sx, centre_of_step.1 + sy);
                        let cost = cost_half(dx, dy);
                        if cost < best.0 {
                            best = (cost, dx, dy);
                        }
                    }
                    if (best.1, best.2) == centre_of_step {
                        break;
                    }
                }
                let at_full = |dx: i32, dy: i32| {
                    sad(
                        centre.full(),
                        other.full(),
                        bx * BLOCK,
                        by * BLOCK,
                        BLOCK,
                        dx,
                        dy,
                    )
                };
                let mut result = Motion {
                    dx: 2 * best.1,
                    dy: 2 * best.2,
                    sad: u32::MAX,
                };
                for sy in -1..=1 {
                    for sx in -1..=1 {
                        let (dx, dy) = (2 * best.1 + sx, 2 * best.2 + sy);
                        let value = at_full(dx, dy);
                        if value + 2 * (sx.unsigned_abs() + sy.unsigned_abs()) < result.sad {
                            result = Motion { dx, dy, sad: value };
                        }
                    }
                }
                result.sad = at_full(result.dx, result.dy);
                // Noise gives small differences between shifts. Then the frame is more likely still.
                let zero = at_full(0, 0);
                if zero <= result.sad + ZERO_BIAS {
                    result = Motion {
                        dx: 0,
                        dy: 0,
                        sad: zero,
                    };
                }
                result
            })
            .collect()
    };
    #[cfg(feature = "parallel")]
    let rows_of_motion: Vec<Vec<Motion>> = (0..rows).into_par_iter().map(fine_row).collect();
    #[cfg(not(feature = "parallel"))]
    let rows_of_motion: Vec<Vec<Motion>> = (0..rows).map(fine_row).collect();
    rows_of_motion.into_iter().flatten().collect()
}

/// The weight table for the knee `t` (levels): index `difference + 255`.
fn weight_table(t: f32) -> [i32; 511] {
    let mut table = [0i32; 511];
    for (index, slot) in table.iter_mut().enumerate() {
        let a = (index as i32 - 255).unsigned_abs() as f32;
        *slot = if a < t {
            FULL
        } else if a < 3.0 * t {
            ((3.0 * t - a) * FULL as f32 / (2.0 * a)) as i32
        } else {
            0
        };
    }
    table
}

/// The limit of a block for its SAD `sad` and the cap `cap`.
fn block_limit(sad: u32, cap: f64) -> i32 {
    let (s, c) = (f64::from(sad), cap);
    if s < c {
        (f64::from(FULL) * (c * c - s * s) / (c * c + s * s)) as i32
    } else {
        0
    }
}

/// The cap of a frame at `distance` from the centre of a window with `reach` as its largest distance, for the knee `t`.
fn frame_cap(t: f32, distance: usize, reach: usize) -> f64 {
    let x = if distance < 2 || reach < 2 {
        0.0
    } else {
        let d = distance.min(reach);
        (1.0 - (((d - 1) as f64) * std::f64::consts::PI / ((reach - 1) as f64)).cos()) / 2.0
    };
    // The SAD of a block of 16 x 16 pixels with a mean absolute difference of t is 256 t.
    f64::from(t) * (BLOCK * BLOCK) as f64 * (NEAR_CAP * (1.0 - x) + x)
}

/// What the stack needs of one other frame.
struct Neighbour<'a> {
    frame: &'a Frame,
    motion: Vec<Motion>,
    cap: f64,
}

/// The weighted mean of the pixels of one plane.
struct Stack<'a> {
    width: usize,
    height: usize,
    /// The size of the block in this plane, and the shift divisor (1 for luma, 2 for chroma).
    block: usize,
    divisor: i32,
    cols: usize,
    neighbours: &'a [Neighbour<'a>],
    plane_of: fn(&Frame) -> &[u8],
    table: &'a [i32; 511],
    cap_factor: f64,
    limit: i32,
}

impl Stack<'_> {
    /// Fills the rows `by * block ..` of `out` for one row of blocks.
    fn row(&self, by: usize, centre: &[u8], out: &mut [u8]) {
        let (w, h, bs) = (self.width, self.height, self.block);
        let y0 = by * bs;
        let y1 = (y0 + bs).min(h);
        for bx in 0..self.cols {
            let x0 = bx * bs;
            let x1 = (x0 + bs).min(w);
            // For this block: the planes, the shifts and the limit of each other frame.
            let blocks: Vec<(&[u8], i32, i32, i32)> = self
                .neighbours
                .iter()
                .filter_map(|n| {
                    let m = n.motion[by * self.cols + bx];
                    let limit = block_limit(m.sad, n.cap * self.cap_factor);
                    (limit > 0).then(|| {
                        (
                            (self.plane_of)(n.frame),
                            m.dx / self.divisor,
                            m.dy / self.divisor,
                            limit,
                        )
                    })
                })
                .collect();
            for y in y0..y1 {
                for x in x0..x1 {
                    let c = i32::from(centre[y * w + x]);
                    let (mut sum_w, mut sum_v) = (FULL, FULL * c);
                    for &(plane, dx, dy, limit) in &blocks {
                        let nx = (x as i32 + dx).clamp(0, w as i32 - 1) as usize;
                        let ny = (y as i32 + dy).clamp(0, h as i32 - 1) as usize;
                        let v = i32::from(plane[ny * w + nx]);
                        let weight = self.table[(v - c + 255) as usize].min(limit);
                        sum_w += weight;
                        sum_v += weight * v;
                    }
                    let mean = (sum_v + sum_w / 2) / sum_w;
                    out[(y - y0) * w + x] = mean.clamp(c - self.limit, c + self.limit) as u8;
                }
            }
        }
    }
}

/// The denoise of a video: push the frames in order, and take the filtered frames in the same order.
///
/// The filtered frame n is ready when the frame n + [`RADIUS`] has been pushed, and the last frames are ready after
/// [`Denoiser::finish`]. The frames are planar YUV 4:2:0 (`width x height` of Y, then U, then V).
pub struct Denoiser {
    width: usize,
    height: usize,
    strength: f32,
    frames: VecDeque<Frame>,
    /// The index of `frames[0]`, and of the next frame to give.
    first: usize,
    next: usize,
    pushed: usize,
    ended: bool,
}

impl Denoiser {
    /// A denoise for frames of `width` x `height` pixels. `strength` multiplies the knee and the limit; 1 is the
    /// setting of the library.
    pub fn new(width: usize, height: usize, strength: f32) -> Result<Denoiser> {
        if width < 4 * BLOCK
            || height < 4 * BLOCK
            || !width.is_multiple_of(2)
            || !height.is_multiple_of(2)
        {
            return invalid(format!("the denoise needs an even frame size of at least {} pixels, not {width} x {height}", 4 * BLOCK));
        }
        if !(strength > 0.0 && strength <= 16.0) {
            return invalid("the strength of the denoise must be more than 0 and at most 16");
        }
        Ok(Denoiser {
            width,
            height,
            strength,
            frames: VecDeque::new(),
            first: 0,
            next: 0,
            pushed: 0,
            ended: false,
        })
    }

    /// The number of bytes of a frame.
    pub fn frame_bytes(&self) -> usize {
        self.width * self.height * 3 / 2
    }

    /// Adds the next frame. If a filtered frame is ready, it replaces the content of `out` and the result is `true`.
    pub fn push(&mut self, frame: &[u8], out: &mut Vec<u8>) -> Result<bool> {
        if self.ended {
            return invalid("the denoise has finished");
        }
        if frame.len() != self.frame_bytes() {
            return invalid(format!(
                "the frame has {} bytes, not {}",
                frame.len(),
                self.frame_bytes()
            ));
        }
        self.frames
            .push_back(Frame::new(frame, self.width, self.height));
        self.pushed += 1;
        if self.next + RADIUS < self.pushed {
            self.filter(out);
            return Ok(true);
        }
        Ok(false)
    }

    /// Gives the next filtered frame after the last frame has been pushed. The result is `false` when there is none.
    pub fn finish(&mut self, out: &mut Vec<u8>) -> bool {
        self.ended = true;
        if self.next < self.pushed {
            self.filter(out);
            true
        } else {
            false
        }
    }

    fn filter(&mut self, out: &mut Vec<u8>) {
        let c = self.next;
        let (start, end) = (c.saturating_sub(RADIUS), (c + RADIUS + 1).min(self.pushed));
        let reach = (c - start).max(end - c - 1);
        let window: Vec<usize> = (start..end).collect();
        let t = KNEE * self.strength;
        let limit = ((LIMIT as f32 * self.strength).round() as i32).max(1);
        let table = weight_table(t);
        let centre = &self.frames[c - self.first];
        let (w, h) = (self.width, self.height);
        let neighbours: Vec<Neighbour> = window
            .iter()
            .filter(|&&i| i != c)
            .map(|&i| {
                let frame = &self.frames[i - self.first];
                Neighbour {
                    frame,
                    motion: flow(centre, frame),
                    cap: frame_cap(t, i.abs_diff(c), reach),
                }
            })
            .collect();
        out.clear();
        out.resize(self.frame_bytes(), 0);
        let (luma, chroma) = out.split_at_mut(w * h);
        let (cols, rows) = (w.div_ceil(BLOCK), h.div_ceil(BLOCK));
        let stack = |size: usize,
                     divisor: i32,
                     plane_of: fn(&Frame) -> &[u8],
                     cap_factor: f64,
                     limit: i32,
                     width: usize,
                     height: usize| Stack {
            width,
            height,
            block: size,
            divisor,
            cols,
            neighbours: &neighbours,
            plane_of,
            table: &table,
            cap_factor,
            limit,
        };
        let luma_stack = stack(BLOCK, 1, y_of, 1.0, limit, w, h);
        let chroma_limit = ((limit as f32 * CHROMA_LIMIT).round() as i32).max(1);
        let u_stack = stack(BLOCK / 2, 2, u_of, CHROMA_CAP, chroma_limit, w / 2, h / 2);
        let v_stack = stack(BLOCK / 2, 2, v_of, CHROMA_CAP, chroma_limit, w / 2, h / 2);
        let (u_out, v_out) = chroma.split_at_mut(w * h / 4);
        run_rows(&luma_stack, &centre.y, luma, rows);
        run_rows(&u_stack, &centre.u, u_out, rows);
        run_rows(&v_stack, &centre.v, v_out, rows);
        self.next += 1;
        while self.first + RADIUS < self.next {
            self.frames.pop_front();
            self.first += 1;
        }
    }
}

fn y_of(frame: &Frame) -> &[u8] {
    &frame.y
}

fn u_of(frame: &Frame) -> &[u8] {
    &frame.u
}

fn v_of(frame: &Frame) -> &[u8] {
    &frame.v
}

/// Runs the stack for all rows of blocks. A row of blocks writes `block` rows of the plane.
fn run_rows(stack: &Stack, centre: &[u8], out: &mut [u8], rows: usize) {
    let band = stack.block * stack.width;
    let task = |(by, chunk): (usize, &mut [u8])| {
        let mut temp = vec![0u8; band];
        stack.row(by, centre, &mut temp);
        chunk.copy_from_slice(&temp[..chunk.len()]);
    };
    #[cfg(feature = "parallel")]
    out.par_chunks_mut(band)
        .enumerate()
        .take(rows)
        .for_each(task);
    #[cfg(not(feature = "parallel"))]
    out.chunks_mut(band).enumerate().take(rows).for_each(task);
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: usize = 128;
    const H: usize = 96;

    /// A small generator of noise and a smooth test image with texture.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> f32 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((self.0 >> 33) as f32) / (1u64 << 31) as f32
        }
        /// About normal with standard deviation 1 (sum of 12 uniform numbers).
        fn normal(&mut self) -> f32 {
            (0..12).map(|_| self.next()).sum::<f32>() - 6.0
        }
    }

    fn scene(shift: i32) -> Vec<f32> {
        (0..W * H)
            .map(|i| {
                let (x, y) = ((i % W) as i32 - shift, (i / W) as i32);
                100.0
                    + 40.0 * ((x as f32) * 0.21).sin() * ((y as f32) * 0.17).cos()
                    + 0.4 * x as f32
            })
            .collect()
    }

    fn frame_of(luma: &[f32], noise: f32, rng: &mut Rng) -> Vec<u8> {
        let mut bytes: Vec<u8> = luma
            .iter()
            .map(|v| (v + noise * rng.normal()).round().clamp(0.0, 255.0) as u8)
            .collect();
        bytes.extend(std::iter::repeat_n(128u8, W * H / 2));
        bytes
    }

    fn run(frames: &[Vec<u8>], strength: f32) -> Vec<Vec<u8>> {
        let mut denoiser = Denoiser::new(W, H, strength).unwrap();
        let mut out = Vec::new();
        let mut result = Vec::new();
        for frame in frames {
            if denoiser.push(frame, &mut out).unwrap() {
                result.push(out.clone());
            }
        }
        while denoiser.finish(&mut out) {
            result.push(out.clone());
        }
        result
    }

    fn rms(a: &[u8], b: &[f32]) -> f32 {
        (a.iter()
            .zip(b)
            .map(|(x, y)| (f32::from(*x) - y).powi(2))
            .sum::<f32>()
            / a.len() as f32)
            .sqrt()
    }

    #[test]
    fn the_weight_table_is_full_below_the_knee_and_zero_from_three_times_the_knee() {
        let table = weight_table(10.0);
        assert_eq!(table[255], FULL);
        assert_eq!(table[255 + 9], FULL);
        assert_eq!(table[255 + 10], FULL);
        assert!(table[255 + 20] > 0 && table[255 + 20] < FULL);
        assert_eq!(table[255 + 30], 0);
        assert_eq!(table[255 - 40], 0);
        assert!(table.windows(2).take(255).all(|p| p[0] <= p[1]));
    }

    #[test]
    fn the_limit_of_a_block_falls_from_256_to_0_at_the_cap() {
        assert_eq!(block_limit(0, 1000.0), 256);
        assert_eq!(block_limit(1000, 1000.0), 0);
        assert_eq!(block_limit(5000, 1000.0), 0);
        assert!(block_limit(500, 1000.0) > 100 && block_limit(500, 1000.0) < 256);
    }

    #[test]
    fn the_nearest_frames_get_a_higher_cap_than_the_farthest() {
        let near = frame_cap(8.0, 1, 3);
        let far = frame_cap(8.0, 3, 3);
        assert!((near / far - NEAR_CAP).abs() < 1e-9);
        assert_eq!(frame_cap(8.0, 2, 3), frame_cap(8.0, 2, 3));
        assert!(near > frame_cap(8.0, 2, 3) && frame_cap(8.0, 2, 3) > far);
    }

    #[test]
    fn every_frame_comes_out_once_in_order() {
        let mut rng = Rng(1);
        let clean = scene(0);
        for count in [1usize, 2, 4, 9] {
            let frames: Vec<Vec<u8>> = (0..count)
                .map(|_| frame_of(&clean, 2.0, &mut rng))
                .collect();
            assert_eq!(run(&frames, 1.0).len(), count);
        }
    }

    #[test]
    fn a_static_noisy_scene_gets_less_noise_and_the_same_brightness() {
        let mut rng = Rng(7);
        let clean = scene(0);
        let frames: Vec<Vec<u8>> = (0..9).map(|_| frame_of(&clean, 4.0, &mut rng)).collect();
        let result = run(&frames, 1.0);
        let before = rms(&frames[4][..W * H], &clean);
        let after = rms(&result[4][..W * H], &clean);
        assert!(after < 0.75 * before, "noise {before} -> {after}");
        let mean =
            |a: &[u8]| a[..W * H].iter().map(|v| f32::from(*v)).sum::<f32>() / (W * H) as f32;
        assert!((mean(&result[4]) - mean(&frames[4])).abs() < 0.5);
    }

    #[test]
    fn a_moving_scene_is_averaged_along_the_motion() {
        let mut rng = Rng(11);
        let frames: Vec<Vec<u8>> = (0..9)
            .map(|i| frame_of(&scene(3 * i), 4.0, &mut rng))
            .collect();
        let result = run(&frames, 1.0);
        let clean = scene(12);
        let (before, after) = (
            rms(&frames[4][..W * H], &clean),
            rms(&result[4][..W * H], &clean),
        );
        assert!(after < 0.85 * before, "noise {before} -> {after}");
    }

    #[test]
    fn a_frame_that_does_not_match_gets_no_weight() {
        let mut rng = Rng(13);
        let clean = scene(0);
        let other: Vec<f32> = clean.iter().map(|v| 255.0 - v).collect();
        let mut frames: Vec<Vec<u8>> = (0..7).map(|_| frame_of(&clean, 3.0, &mut rng)).collect();
        frames[5] = frame_of(&other, 3.0, &mut rng);
        let result = run(&frames, 1.0);
        // The frame 5 is from another scene. The result of frame 3 must not carry it.
        assert!(rms(&result[3][..W * H], &clean) < rms(&frames[3][..W * H], &clean));
    }

    #[test]
    fn the_result_stays_within_the_limit_of_the_original() {
        let mut rng = Rng(17);
        let clean = scene(0);
        let frames: Vec<Vec<u8>> = (0..7).map(|_| frame_of(&clean, 6.0, &mut rng)).collect();
        let result = run(&frames, 1.0);
        let limit = LIMIT;
        let worst = result[3][..W * H]
            .iter()
            .zip(&frames[3][..W * H])
            .map(|(a, b)| (i32::from(*a) - i32::from(*b)).abs())
            .max()
            .unwrap();
        assert!(worst <= limit, "worst change {worst}, limit {limit}");
    }

    #[test]
    fn the_flow_finds_a_shift_of_a_block() {
        let mut rng = Rng(19);
        let a = Frame::new(&frame_of(&scene(0), 0.0, &mut rng), W, H);
        let b = Frame::new(&frame_of(&scene(5), 0.0, &mut rng), W, H);
        let motion = flow(&a, &b);
        // The scene moves 5 pixels to the right, thus the pixel x of the first frame is at x + 5 in the other frame.
        let inner = &motion[W / BLOCK + 2];
        assert_eq!((inner.dx, inner.dy), (5, 0), "{inner:?}");
        assert!(inner.sad < 300);
    }

    #[test]
    fn the_constructor_refuses_a_wrong_size_or_strength() {
        assert!(Denoiser::new(30, 96, 1.0).is_err());
        assert!(Denoiser::new(128, 95, 1.0).is_err());
        assert!(Denoiser::new(128, 96, 0.0).is_err());
        let mut denoiser = Denoiser::new(128, 96, 1.0).unwrap();
        assert!(denoiser.push(&[0u8; 10], &mut Vec::new()).is_err());
    }
}
