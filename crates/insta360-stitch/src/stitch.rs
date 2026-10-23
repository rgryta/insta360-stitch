//! This module stitches photos pixel by pixel. For videos, it makes remap planes for ffmpeg.

use crate::error::{unsupported, Result};
use crate::lens::{blend_weight, LensModel, RowMaps};

#[cfg(feature = "parallel")]
use rayon::prelude::*;

/// An interleaved 8-bit image. You can use a 3-channel or 4-channel image in any color space.
/// The sampling is linear, and a linear blend gives the same result before or after an affine color conversion (YCbCr <-> RGB).
#[derive(Debug, Clone, Copy)]
pub struct Image<'a> {
    pub width: usize,
    pub height: usize,
    pub channels: usize,
    pub data: &'a [u8],
}

/// Checks the layout of a raw photo. The two fisheye circles are side by side, and each lens crop is `height` x `height`.
pub fn check_photo_layout(width: usize, height: usize) -> Result<()> {
    if width != 2 * height {
        return unsupported(format!("unexpected .insp layout {width}x{height}"));
    }
    Ok(())
}

fn check_source(source: &Image) -> Result<()> {
    check_photo_layout(source.width, source.height)?;
    if source.channels == 0 || source.data.len() != source.width * source.height * source.channels {
        return unsupported("image data does not match its dimensions");
    }
    Ok(())
}

/// Makes one output row of the stitched photo. The size and channels are the same as the source.
fn stitch_row(source: &Image, model: &LensModel, y: usize, maps: &mut RowMaps, out: &mut [u8]) {
    let (width, height, channels) = (source.width, source.height, source.channels);
    model.row(y, width, height, maps);
    let mut a = [0f32; 4];
    let mut b = [0f32; 4];
    // Bilinear sampling. A neighbor pixel outside the lens crop is black.
    let sample = |lens: usize, u: f32, v: f32, into: &mut [f32; 4]| -> bool {
        *into = [0.0; 4];
        if u.is_nan() {
            return false;
        }
        let (x0, y0) = (u.floor(), v.floor());
        let (fx, fy) = (u - x0, v - y0);
        let (x0, y0) = (x0 as isize, y0 as isize);
        for (dx, dy, weight) in [
            (0, 0, (1.0 - fx) * (1.0 - fy)),
            (1, 0, fx * (1.0 - fy)),
            (0, 1, (1.0 - fx) * fy),
            (1, 1, fx * fy),
        ] {
            let (x, y) = (x0 + dx, y0 + dy);
            if x < 0 || x >= height as isize || y < 0 || y >= height as isize {
                continue;
            }
            let i = (y as usize * width + x as usize + lens * height) * channels;
            for (value, &byte) in into.iter_mut().zip(&source.data[i..i + channels.min(4)]) {
                *value += f32::from(byte) * weight;
            }
        }
        true
    };
    for x in 0..width {
        let valid0 = sample(0, maps.u[0][x], maps.v[0][x], &mut a);
        let valid1 = sample(1, maps.u[1][x], maps.v[1][x], &mut b);
        let weight = blend_weight(valid0, valid1, maps.weight0[x]);
        let o = x * channels;
        for c in 0..channels.min(4) {
            out[o + c] = (a[c] * weight + b[c] * (1.0 - weight) + 0.5) as u8;
        }
    }
}

/// Stitches a raw photo into `out`. The dimensions and channels of `out` are the same as `source`.
pub fn stitch_photo_into(source: &Image, model: &LensModel, out: &mut [u8]) -> Result<()> {
    check_source(source)?;
    if out.len() != source.data.len() {
        return unsupported("output buffer does not match the source");
    }
    let row_len = source.width * source.channels;
    #[cfg(feature = "parallel")]
    out.par_chunks_mut(row_len).enumerate().for_each_init(
        || RowMaps::new(source.width),
        |maps, (y, row)| stitch_row(source, model, y, maps, row),
    );
    #[cfg(not(feature = "parallel"))]
    {
        let mut maps = RowMaps::new(source.width);
        for (y, row) in out.chunks_mut(row_len).enumerate() {
            stitch_row(source, model, y, &mut maps, row);
        }
    }
    Ok(())
}

/// Stitches a raw photo into a new buffer.
pub fn stitch_photo(source: &Image, model: &LensModel) -> Result<Vec<u8>> {
    let mut out = vec![0; source.data.len()];
    stitch_photo_into(source, model, &mut out)?;
    Ok(out)
}

/// Stitches a raw photo row by row. It gives each row to `sink` in the correct order.
/// Thus, a streaming encoder does not need the full output in memory.
/// The function calculates the rows in parallel bands of `band` rows.
pub fn stitch_photo_rows<F>(
    source: &Image,
    model: &LensModel,
    band: usize,
    mut sink: F,
) -> Result<()>
where
    F: FnMut(usize, &[u8]) -> Result<()>,
{
    check_source(source)?;
    let row_len = source.width * source.channels;
    let band = band.max(1);
    let mut buffer = vec![0u8; row_len * band];
    for start in (0..source.height).step_by(band) {
        let rows = band.min(source.height - start);
        let chunk = &mut buffer[..rows * row_len];
        #[cfg(feature = "parallel")]
        chunk.par_chunks_mut(row_len).enumerate().for_each_init(
            || RowMaps::new(source.width),
            |maps, (i, row)| stitch_row(source, model, start + i, maps, row),
        );
        #[cfg(not(feature = "parallel"))]
        {
            let mut maps = RowMaps::new(source.width);
            for (i, row) in chunk.chunks_mut(row_len).enumerate() {
                stitch_row(source, model, start + i, &mut maps, row);
            }
        }
        for (i, row) in chunk.chunks(row_len).enumerate() {
            sink(start + i, row)?;
        }
    }
    Ok(())
}

/// The remap planes for ffmpeg. The output is `width` x `width / 2`, and each lens stream is `width` x `lens_height`.
/// The coordinates are nearest-neighbor. The value 65535 shows a pixel outside that lens.
/// `mask` is the blend weight of lens 0 (0-255).
#[derive(Debug, Clone)]
pub struct VideoMaps {
    pub width: usize,
    pub height: usize,
    pub x: [Vec<u16>; 2],
    pub y: [Vec<u16>; 2],
    pub mask: Vec<u8>,
}

/// The ffmpeg `-filter_complex` that applies [`VideoMaps`].
/// Input 0 is the .insv file. Its first video stream is lens 1.
/// Inputs 1 to 4 are the x0, y0, x1 and y1 planes (`gray16le`). Input 5 is the mask (`gray`).
pub const FFMPEG_FILTER: &str = "[0:v:1]format=gbrp[l0];[0:v:0]format=gbrp[l1];[5]format=gbrp[m];\
[l0][1][2]remap[f];[l1][3][4]remap[b];[b][f][m]maskedmerge,format=yuv420p[out]";

pub fn video_maps(model: &LensModel, width: usize, lens_height: usize) -> Result<VideoMaps> {
    if width == 0 || width % 2 != 0 {
        return unsupported(format!("expected an even lens width, got {width}"));
    }
    let height = width / 2;
    let mut x0 = vec![0u16; width * height];
    let mut y0 = vec![0u16; width * height];
    let mut x1 = vec![0u16; width * height];
    let mut y1 = vec![0u16; width * height];
    let mut mask = vec![0u8; width * height];
    let fill = |y: usize,
                maps: &mut RowMaps,
                x0: &mut [u16],
                y0: &mut [u16],
                x1: &mut [u16],
                y1: &mut [u16],
                mask: &mut [u8]| {
        model.row(y, width, height, maps);
        for x in 0..width {
            let mut valid = [false; 2];
            for (l, (xs, ys)) in [(&mut *x0, &mut *y0), (&mut *x1, &mut *y1)]
                .into_iter()
                .enumerate()
            {
                let (u, v) = (maps.u[l][x].round(), maps.v[l][x].round());
                let ok = u >= 0.0 && u < width as f32 && v >= 0.0 && v < lens_height as f32;
                xs[x] = if ok { u as u16 } else { 65535 };
                ys[x] = if ok { v as u16 } else { 65535 };
                valid[l] = ok;
            }
            mask[x] = (255.0 * blend_weight(valid[0], valid[1], maps.weight0[x])).round() as u8;
        }
    };
    #[cfg(feature = "parallel")]
    x0.par_chunks_mut(width)
        .zip(y0.par_chunks_mut(width))
        .zip(x1.par_chunks_mut(width))
        .zip(y1.par_chunks_mut(width))
        .zip(mask.par_chunks_mut(width))
        .enumerate()
        .for_each_init(
            || RowMaps::new(width),
            |maps, (y, ((((x0, y0), x1), y1), mask))| fill(y, maps, x0, y0, x1, y1, mask),
        );
    #[cfg(not(feature = "parallel"))]
    {
        let mut maps = RowMaps::new(width);
        for y in 0..height {
            let r = y * width..(y + 1) * width;
            fill(
                y,
                &mut maps,
                &mut x0[r.clone()],
                &mut y0[r.clone()],
                &mut x1[r.clone()],
                &mut y1[r.clone()],
                &mut mask[r],
            );
        }
    }
    Ok(VideoMaps {
        width,
        height,
        x: [x0, x1],
        y: [y0, y1],
        mask,
    })
}
