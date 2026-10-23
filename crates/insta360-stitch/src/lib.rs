//! Stitch Insta360 dual-fisheye photos (`.insp`) and videos (`.insv`) into equirectangular
//! panoramas, using the calibration and motion data each file carries in its trailer.
//!
//! The crate does no decoding or encoding and never runs ffmpeg: it reads the trailer from any
//! seekable source, stitches decoded pixels, and produces remap planes a caller feeds to ffmpeg.
//!
//! ```no_run
//! use insta360_stitch::{read_trailer, orientation, LensModel, stitch_photo, Image};
//!
//! let mut file = std::fs::File::open("IMG_20260821_101803_00_013.insp")?;
//! let trailer = read_trailer(&mut file)?;
//! let (width, height, pixels) = (11904, 5952, vec![0u8; 11904 * 5952 * 3]); // decoded JPEG
//! let model = LensModel::new(&trailer, height, orientation(&trailer)?)?;
//! let stitched = stitch_photo(&Image { width, height, channels: 3, data: &pixels }, &model)?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

mod clock;
mod error;
mod lens;
pub mod math;
mod orientation;
mod preview;
mod stitch;
mod trailer;

pub use clock::{camera_utc_offset_minutes, wall_clock_seconds};
pub use error::{Error, Result};
pub use lens::{blend_weight, Lens, LensModel, RowMaps};
pub use orientation::{
    box_gravity_quats, butter2_lowpass, filtfilt2, orientation, Biquad, GravityQuats,
};
pub use preview::preview_rgb;
pub use stitch::{
    check_photo_layout, stitch_photo, stitch_photo_into, stitch_photo_rows, video_maps, Image,
    VideoMaps, FFMPEG_FILTER,
};
pub use trailer::{
    parse_trailer, read_preview, read_trailer, trailer_length, Imu, Preview, Trailer, TAIL_LENGTH,
};

/// Whether a trailer can be stitched (calibration, lens guard and levelling all supported).
pub fn check_stitchable(trailer: &Trailer) -> Result<()> {
    LensModel::new(trailer, 1, orientation(trailer)?).map(|_| ())
}
