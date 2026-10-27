//! This module stitches photos pixel by pixel. For videos, it makes remap planes for ffmpeg.

use crate::error::{invalid, unsupported, Result};
use crate::fusion::{fusion_maps, FusionMaps};
use crate::lens::{blend_weight, output_direction, LensModel, RowMaps};
use crate::math::apply;
use crate::seamflow::SeamFlow;

#[cfg(feature = "parallel")]
use rayon::prelude::*;

/// An interleaved 8-bit image: 1 or 2 channels (grey), or RGB / RGBA. With 3 or 4 channels, the stitch matches the
/// colours of the two lenses at the seam (as Insta360 Studio), and that needs RGB.
#[derive(Debug, Clone, Copy)]
pub struct Image<'a> {
    pub width: usize,
    pub height: usize,
    pub channels: usize,
    pub data: &'a [u8],
}

/// Checks the layout of a raw photo. The two fisheye circles are side by side, and each lens crop is `height` x `height`.
pub fn check_photo_layout(width: usize, height: usize) -> Result<()> {
    if height == 0 || width != 2 * height {
        return unsupported(format!("unexpected .insp layout {width}x{height}"));
    }
    Ok(())
}

pub(crate) fn check_source(source: &Image) -> Result<()> {
    check_photo_layout(source.width, source.height)?;
    let size = source
        .width
        .checked_mul(source.height)
        .and_then(|pixels| pixels.checked_mul(source.channels));
    if !(1..=4).contains(&source.channels) || size != Some(source.data.len()) {
        return invalid("image data does not match its dimensions");
    }
    Ok(())
}

/// Bilinear sample of lens `lens` of a photo at (u, v) (lens crop pixels). A neighbor pixel outside the lens crop is
/// black. Gives `false` for NaN (outside the lens).
fn sample(source: &Image, lens: usize, u: f32, v: f32, into: &mut [f32; 4]) -> bool {
    let (width, height, channels) = (source.width, source.height, source.channels);
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
}

/// The colour fusion of an RGB or RGBA photo; `None` for grey.
fn photo_fusion(source: &Image, model: &LensModel) -> Option<FusionMaps> {
    (source.channels >= 3).then(|| {
        fusion_maps(model, source.height, |lens, u, v| {
            let mut c = [0f32; 4];
            sample(source, lens, u as f32, v as f32, &mut c);
            [c[0], c[1], c[2]]
        })
    })
}

/// The colour fusion and the seam flow of one photo with one lens model (as Studio, one time for each photo).
/// Make it one time, and give it to [`stitch_photo_band_with`] for each band of the photo.
pub struct PhotoSeam {
    fusion: Option<FusionMaps>,
    flow: Option<SeamFlow>,
}

impl PhotoSeam {
    pub fn new(source: &Image, model: &LensModel) -> Result<Self> {
        check_source(source)?;
        let flow = SeamFlow::new(model, source.height as f64, |lens, u, v| {
            let mut c = [0f32; 4];
            sample(source, lens, u, v, &mut c);
            if source.channels >= 3 {
                0.299 * c[0] + 0.587 * c[1] + 0.114 * c[2]
            } else {
                c[0]
            }
        });
        Ok(PhotoSeam {
            fusion: photo_fusion(source, model),
            flow,
        })
    }
}

/// Makes one output row of the stitched photo. The size and channels are the same as the source.
fn stitch_row(
    source: &Image,
    model: &LensModel,
    seam: &PhotoSeam,
    y: usize,
    maps: &mut RowMaps,
    out: &mut [u8],
) {
    let (width, height, channels) = (source.width, source.height, source.channels);
    model.row(y, width, height, maps);
    let mut a = [0f32; 4];
    let mut b = [0f32; 4];
    for x in 0..width {
        let d = apply(&model.level, &output_direction(x, y, width, height));
        let mut uv = [(maps.u[0][x], maps.v[0][x]), (maps.u[1][x], maps.v[1][x])];
        let mut alpha = maps.weight0[x];
        if let Some(shift) = seam.flow.as_ref().and_then(|f| f.shift(&d)) {
            for l in 0..2 {
                uv[l].0 += shift[l].0;
                uv[l].1 += shift[l].1;
            }
            // The shader samples the lens masks at the moved positions.
            if let (Some(blend), false) = (&model.blend, uv[0].0.is_nan() || uv[1].0.is_nan()) {
                let pos = uv.map(|(u, v)| (f64::from(u), f64::from(v)));
                alpha = blend.weight(&d, pos) as f32;
            }
        }
        let valid0 = sample(source, 0, uv[0].0, uv[0].1, &mut a);
        let valid1 = sample(source, 1, uv[1].0, uv[1].1, &mut b);
        if let Some(fusion) = &seam.fusion {
            // As the Insta360 shader: c' = (c + 1) * gain - 1 on colours of 0 to 1, before the blend.
            for (l, c) in [(0, &mut a), (1, &mut b)] {
                let g = fusion.gain_at(l, &d);
                for k in 0..3 {
                    c[k] = ((c[k] + 255.0) * g[k] - 255.0).clamp(0.0, 255.0);
                }
            }
        }
        let weight = blend_weight(valid0, valid1, alpha);
        let o = x * channels;
        for c in 0..channels.min(4) {
            out[o + c] = (a[c] * weight + b[c] * (1.0 - weight) + 0.5) as u8;
        }
    }
}

fn check_band(source: &Image, first_row: usize, out: &[u8]) -> Result<()> {
    check_source(source)?;
    let row_len = source.width * source.channels;
    let end = first_row.checked_add(out.len() / row_len);
    if !out.len().is_multiple_of(row_len) || end.is_none_or(|end| end > source.height) {
        return invalid("the band is outside the photo, or it is not full rows");
    }
    Ok(())
}

/// Stitches the rows `first_row..first_row + n` of a raw photo into `out`.
/// `out` must contain exactly n full rows, with the same width and channels as `source`.
/// Each call calculates the [`PhotoSeam`] again. For many bands of one photo, use [`stitch_photo_band_with`].
pub fn stitch_photo_band(
    source: &Image,
    model: &LensModel,
    first_row: usize,
    out: &mut [u8],
) -> Result<()> {
    check_band(source, first_row, out)?;
    stitch_photo_band_with(
        source,
        model,
        &PhotoSeam::new(source, model)?,
        first_row,
        out,
    )
}

/// As [`stitch_photo_band`], with the seam of this photo and model.
pub fn stitch_photo_band_with(
    source: &Image,
    model: &LensModel,
    seam: &PhotoSeam,
    first_row: usize,
    out: &mut [u8],
) -> Result<()> {
    check_band(source, first_row, out)?;
    let row_len = source.width * source.channels;
    #[cfg(feature = "parallel")]
    out.par_chunks_mut(row_len).enumerate().for_each_init(
        || RowMaps::new(source.width),
        |maps, (i, row)| stitch_row(source, model, seam, first_row + i, maps, row),
    );
    #[cfg(not(feature = "parallel"))]
    {
        let mut maps = RowMaps::new(source.width);
        for (i, row) in out.chunks_mut(row_len).enumerate() {
            stitch_row(source, model, seam, first_row + i, &mut maps, row);
        }
    }
    Ok(())
}

/// Stitches a raw photo into `out`. The dimensions and channels of `out` are the same as `source`.
pub fn stitch_photo_into(source: &Image, model: &LensModel, out: &mut [u8]) -> Result<()> {
    if out.len() != source.data.len() {
        return invalid("output buffer does not match the source");
    }
    stitch_photo_band(source, model, 0, out)
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
    let band = band.clamp(1, source.height);
    let mut buffer = vec![0u8; row_len * band];
    let seam = PhotoSeam::new(source, model)?;
    for start in (0..source.height).step_by(band) {
        let rows = band.min(source.height - start);
        let chunk = &mut buffer[..rows * row_len];
        stitch_photo_band_with(source, model, &seam, start, chunk)?;
        for (i, row) in chunk.chunks(row_len).enumerate() {
            sink(start + i, row)?;
        }
    }
    Ok(())
}

/// The remap planes for ffmpeg, for all output rows or for one band of rows.
/// The output is `width` pixels wide. The coordinates are nearest-neighbor pixels in the lens streams.
/// The value 65535 shows a pixel outside that lens. `mask` is the blend weight of lens 0 (0-255).
#[derive(Debug, Clone)]
pub struct VideoMaps {
    pub width: usize,
    /// The number of rows in these maps.
    pub height: usize,
    pub x: [Vec<u16>; 2],
    pub y: [Vec<u16>; 2],
    pub mask: Vec<u8>,
}

impl VideoMaps {
    /// Gives the x0, y0, x1 and y1 planes as little-endian bytes (`gray16le`), the format that ffmpeg reads.
    pub fn le_bytes(&self) -> [Vec<u8>; 4] {
        [&self.x[0], &self.y[0], &self.x[1], &self.y[1]]
            .map(|plane| plane.iter().flat_map(|value| value.to_le_bytes()).collect())
    }
}

/// The largest output width of the video maps. At this width, the maps use approximately 1.2 GB.
pub const MAX_VIDEO_WIDTH: usize = 16384;

/// The ffmpeg `-filter_complex` that applies [`VideoMaps`].
/// Input 0 is the .insv file. Its first video stream is lens 1.
/// Inputs 1 to 4 are the x0, y0, x1 and y1 planes (`gray16le`). Input 5 is the mask (`gray`).
pub const FFMPEG_FILTER: &str = "[0:v:1]format=gbrp[l0];[0:v:0]format=gbrp[l1];[5]format=gbrp[m];\
[l0][1][2]remap[f];[l1][3][4]remap[b];[b][f][m]maskedmerge,format=yuv420p[out]";

fn check_video(width: usize, lens_width: usize, lens_height: usize) -> Result<()> {
    // The lens model is for a square lens stream. The maps keep coordinates as u16, and 65535 marks a pixel outside the lens.
    if lens_width == 0 || lens_width != lens_height || lens_width >= 65535 {
        return unsupported(format!(
            "unsupported lens stream size {lens_width}x{lens_height}"
        ));
    }
    // yuv420p needs an even height, thus the width (two times the height) must be a multiple of 4.
    if width == 0 || !width.is_multiple_of(4) || width > MAX_VIDEO_WIDTH {
        return invalid(format!(
            "the output width must be a multiple of 4 and at most {MAX_VIDEO_WIDTH}, not {width}"
        ));
    }
    Ok(())
}

/// Calculates the rows `first_row..first_row + rows` of the remap planes.
fn video_band(
    model: &LensModel,
    width: usize,
    lens_width: usize,
    lens_height: usize,
    first_row: usize,
    rows: usize,
) -> VideoMaps {
    let height = width / 2;
    let mut x0 = vec![0u16; width * rows];
    let mut y0 = vec![0u16; width * rows];
    let mut x1 = vec![0u16; width * rows];
    let mut y1 = vec![0u16; width * rows];
    let mut mask = vec![0u8; width * rows];
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
                let ok = u >= 0.0 && u < lens_width as f32 && v >= 0.0 && v < lens_height as f32;
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
            |maps, (i, ((((x0, y0), x1), y1), mask))| {
                fill(first_row + i, maps, x0, y0, x1, y1, mask)
            },
        );
    #[cfg(not(feature = "parallel"))]
    {
        let mut maps = RowMaps::new(width);
        for i in 0..rows {
            let r = i * width..(i + 1) * width;
            fill(
                first_row + i,
                &mut maps,
                &mut x0[r.clone()],
                &mut y0[r.clone()],
                &mut x1[r.clone()],
                &mut y1[r.clone()],
                &mut mask[r],
            );
        }
    }
    VideoMaps {
        width,
        height: rows,
        x: [x0, x1],
        y: [y0, y1],
        mask,
    }
}

/// Makes the remap planes for an equirectangular output of `width` x `width / 2`.
/// Each lens stream is `lens_width` x `lens_height`. Make `model` for a lens size of `lens_width`.
/// A good output width is `2 * lens_width`: it keeps approximately the resolution of the lenses.
/// The width must be a multiple of 4 and at most [`MAX_VIDEO_WIDTH`]. The lens stream must be square.
pub fn video_maps(
    model: &LensModel,
    width: usize,
    lens_width: usize,
    lens_height: usize,
) -> Result<VideoMaps> {
    check_video(width, lens_width, lens_height)?;
    Ok(video_band(
        model,
        width,
        lens_width,
        lens_height,
        0,
        width / 2,
    ))
}

/// Makes the remap planes in bands of `band` rows, and gives each band to `sink` with its first row.
/// Thus, the caller can write the planes to files without the full maps in memory.
pub fn video_maps_rows<F>(
    model: &LensModel,
    width: usize,
    lens_width: usize,
    lens_height: usize,
    band: usize,
    mut sink: F,
) -> Result<()>
where
    F: FnMut(usize, &VideoMaps) -> Result<()>,
{
    check_video(width, lens_width, lens_height)?;
    let height = width / 2;
    let band = band.clamp(1, height);
    for first_row in (0..height).step_by(band) {
        let rows = band.min(height - first_row);
        sink(
            first_row,
            &video_band(model, width, lens_width, lens_height, first_row, rows),
        )?;
    }
    Ok(())
}
