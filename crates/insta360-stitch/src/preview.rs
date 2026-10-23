use crate::trailer::Preview;

/// The camera's NV12 preview as packed RGB (BT.601, limited range).
pub fn preview_rgb(preview: &Preview) -> Vec<u8> {
    let (width, height) = (preview.width as usize, preview.height as usize);
    let nv12 = &preview.nv12;
    let clamp = |v: f64| v.round().clamp(0.0, 255.0) as u8;
    let mut rgb = vec![0u8; width * height * 3];
    for y in 0..height {
        for x in 0..width {
            let luma = 1.164 * (f64::from(nv12[y * width + x]) - 16.0);
            let uv = width * height + (y >> 1) * width + (x & !1);
            let u = f64::from(nv12[uv]) - 128.0;
            let v = f64::from(nv12[uv + 1]) - 128.0;
            let o = (y * width + x) * 3;
            rgb[o] = clamp(luma + 1.596 * v);
            rgb[o + 1] = clamp(luma - 0.392 * u - 0.813 * v);
            rgb[o + 2] = clamp(luma + 2.017 * u);
        }
    }
    rgb
}
