use std::cell::RefCell;
use std::fs::File;
use std::io::{BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use clap::{Parser, Subcommand};
use image::{ImageFormat, ImageReader};
use insta360_stitch::{
    check_photo_layout, check_stitchable, orientation, preview_rgb, read_preview, read_trailer,
    stitch_photo_band, video_maps, Image, LensModel, Trailer, FFMPEG_FILTER,
};
use jpeg_encoder::{rgb_to_ycbcr, Encoder, ImageBuffer, JpegColorType};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Stitches Insta360 dual-fisheye photos (.insp) and videos (.insv) into equirectangular panoramas.
#[derive(Parser)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Shows the data in the trailer of a file, as JSON
    Info { file: PathBuf },
    /// Stitches a .insp photo into an equirectangular JPEG
    Photo {
        input: PathBuf,
        output: PathBuf,
        #[arg(long, default_value_t = 95)]
        quality: u8,
    },
    /// Writes the small preview that the camera stitched, as a JPEG
    Preview { input: PathBuf, output: PathBuf },
    /// Stitches a .insv video into an equirectangular MP4. This command needs ffmpeg and ffprobe
    Video {
        input: PathBuf,
        output: PathBuf,
        #[arg(long, default_value_t = 20)]
        crf: u8,
        #[arg(long, default_value = "medium")]
        preset: String,
        #[arg(long, default_value = "ffmpeg")]
        ffmpeg: String,
        #[arg(long, default_value = "ffprobe")]
        ffprobe: String,
    },
}

fn main() -> ExitCode {
    let result = match Cli::parse().command {
        Commands::Info { file } => info(&file),
        Commands::Photo {
            input,
            output,
            quality,
        } => photo(&input, &output, quality),
        Commands::Preview { input, output } => preview(&input, &output),
        Commands::Video {
            input,
            output,
            crf,
            preset,
            ffmpeg,
            ffprobe,
        } => video(&input, &output, crf, &preset, &ffmpeg, &ffprobe),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("insta360-stitch: {error}");
            ExitCode::FAILURE
        }
    }
}

fn trailer_of(path: &Path) -> Result<Trailer> {
    Ok(read_trailer(&mut BufReader::new(File::open(path)?))?)
}

fn info(path: &Path) -> Result<()> {
    let trailer = trailer_of(path)?;
    let stitchable = check_stitchable(&trailer);
    let offset = &trailer.offset;
    let json = serde_json::json!({
        "stitchable": stitchable.is_ok(),
        "reason": stitchable.err().map(|error| error.to_string()),
        "lensType": offset[15] as u32 & 0xff,
        "offsetState": trailer.offset_state,
        "calibration": { "width": offset[13], "height": offset[14] },
        "localTime": trailer.local_time,
        "firstFrameUs": trailer.first_frame_us,
        "imuSamples": trailer.imu.as_ref().map_or(0, |imu| imu.len()),
        "preview": trailer.preview_size().map(|(width, height)| serde_json::json!({ "width": width, "height": height })),
    });
    println!("{}", serde_json::to_string_pretty(&json)?);
    Ok(())
}

/// The number of output rows that the CLI stitches at one time.
const BAND_ROWS: usize = 64;

/// The RGB rows that the JPEG encoder reads. The encoder reads the rows in sequence.
/// For a photo, the CLI stitches one band of rows at a time. Thus, it does not keep the full output in memory.
enum Rows<'a> {
    Plain {
        rgb: &'a [u8],
        width: usize,
        height: usize,
    },
    Stitched {
        source: Image<'a>,
        model: &'a LensModel,
        /// The first row of the band, and the stitched band.
        band: RefCell<(usize, Vec<u8>)>,
    },
}

impl Rows<'_> {
    fn size(&self) -> (usize, usize) {
        match self {
            Rows::Plain { width, height, .. } => (*width, *height),
            Rows::Stitched { source, .. } => (source.width, source.height),
        }
    }
}

impl ImageBuffer for Rows<'_> {
    fn get_jpeg_color_type(&self) -> JpegColorType {
        JpegColorType::Ycbcr
    }

    fn width(&self) -> u16 {
        self.size().0 as u16
    }

    fn height(&self) -> u16 {
        self.size().1 as u16
    }

    fn fill_buffers(&self, y: u16, buffers: &mut [Vec<u8>; 4]) {
        let (width, height) = self.size();
        let (y, row_len) = (usize::from(y), width * 3);
        let mut push = |row: &[u8]| {
            for pixel in row.as_chunks::<3>().0 {
                let (luma, cb, cr) = rgb_to_ycbcr(pixel[0], pixel[1], pixel[2]);
                buffers[0].push(luma);
                buffers[1].push(cb);
                buffers[2].push(cr);
            }
        };
        match self {
            Rows::Plain { rgb, .. } => push(&rgb[y * row_len..][..row_len]),
            Rows::Stitched {
                source,
                model,
                band,
            } => {
                let mut band = band.borrow_mut();
                let (first, rows) = (band.0, band.1.len() / row_len);
                if y < first || y >= first + rows {
                    let rows = BAND_ROWS.min(height - y);
                    band.0 = y;
                    band.1.resize(rows * row_len, 0);
                    // The CLI checked the source and the model before it started the encoder.
                    stitch_photo_band(source, model, y, &mut band.1).expect("a valid source");
                }
                push(&band.1[(y - band.0) * row_len..][..row_len]);
            }
        }
    }
}

/// Writes a JPEG with an XMP GPano packet. Other viewers then open it as a 360 panorama.
fn write_panorama_jpeg(output: &Path, rows: Rows, quality: u8) -> Result<()> {
    let (width, height) = rows.size();
    if width > usize::from(u16::MAX) || height > usize::from(u16::MAX) {
        return Err(format!(
            "a JPEG cannot be larger than 65535 pixels, the image is {width}x{height}"
        )
        .into());
    }
    let xmp = format!(
        concat!(
            "http://ns.adobe.com/xap/1.0/\0",
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">"#,
            r#"<rdf:Description rdf:about="" xmlns:GPano="http://ns.google.com/photos/1.0/panorama/" "#,
            r#"GPano:ProjectionType="equirectangular" GPano:UsePanoramaViewer="True" "#,
            r#"GPano:FullPanoWidthPixels="{w}" GPano:FullPanoHeightPixels="{h}" "#,
            r#"GPano:CroppedAreaImageWidthPixels="{w}" GPano:CroppedAreaImageHeightPixels="{h}" "#,
            r#"GPano:CroppedAreaLeftPixels="0" GPano:CroppedAreaTopPixels="0"/></rdf:RDF></x:xmpmeta>"#
        ),
        w = width,
        h = height
    );
    let mut out = BufWriter::new(File::create(output)?);
    let mut encoder = Encoder::new(&mut out, quality);
    encoder.add_app_segment(1, xmp.into_bytes())?;
    encoder.encode_image(rows)?;
    out.flush()?;
    Ok(())
}

fn photo(input: &Path, output: &Path, quality: u8) -> Result<()> {
    let trailer = trailer_of(input)?;
    // A .insp file is a usual JPEG with the trailer at the end.
    let decoded = ImageReader::with_format(BufReader::new(File::open(input)?), ImageFormat::Jpeg)
        .decode()?
        .into_rgb8();
    let (width, height) = (decoded.width() as usize, decoded.height() as usize);
    check_photo_layout(width, height)?;
    let model = LensModel::new(&trailer, height, orientation(&trailer)?)?;
    let source = Image {
        width,
        height,
        channels: 3,
        data: decoded.as_raw(),
    };
    let rows = Rows::Stitched {
        source,
        model: &model,
        band: RefCell::new((0, Vec::new())),
    };
    write_panorama_jpeg(output, rows, quality)
}

fn preview(input: &Path, output: &Path) -> Result<()> {
    let mut reader = BufReader::new(File::open(input)?);
    let trailer = read_trailer(&mut reader)?;
    let preview = read_preview(&mut reader, &trailer)?;
    let rgb = preview_rgb(&preview);
    let rows = Rows::Plain {
        rgb: &rgb,
        width: preview.width as usize,
        height: preview.height as usize,
    };
    write_panorama_jpeg(output, rows, 92)
}

/// Makes a new, empty temporary folder with a unique name.
/// `create_dir` fails if the path exists, also if it is a symbolic link. Thus, the CLI does not use a folder of a different process.
fn new_temp_folder() -> Result<PathBuf> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |time| time.as_nanos());
    for attempt in 0..100 {
        let name = format!("insta360-stitch-{}-{nanos}-{attempt}", std::process::id());
        let folder = std::env::temp_dir().join(name);
        match std::fs::create_dir(&folder) {
            Ok(()) => return Ok(folder),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Err("cannot make a temporary folder".into())
}

/// Gives the size of the first video stream. A .insv file that the library can stitch has two streams, one for each lens.
fn lens_stream(ffprobe: &str, input: &Path) -> Result<(usize, usize)> {
    let out = Command::new(ffprobe)
        .args([
            "-v",
            "error",
            "-select_streams",
            "v",
            "-show_entries",
            "stream=width,height",
            "-of",
            "csv=p=0",
        ])
        .arg(input)
        .output()?;
    if !out.status.success() {
        return Err(format!(
            "ffprobe failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )
        .into());
    }
    let streams: Vec<(usize, usize)> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let (w, h) = line.trim().split_once(',')?;
            Some((w.parse().ok()?, h.parse().ok()?))
        })
        .collect();
    match streams.as_slice() {
        [first, _, ..] => Ok(*first),
        _ => Err("expected one video stream per lens".into()),
    }
}

fn video(
    input: &Path,
    output: &Path,
    crf: u8,
    preset: &str,
    ffmpeg: &str,
    ffprobe: &str,
) -> Result<()> {
    let trailer = trailer_of(input)?;
    let (width, lens_height) = lens_stream(ffprobe, input)?;
    // shortcut: the video is level only at the first frame. To level each frame, the stitcher needs maps for each frame.
    let model = LensModel::new(&trailer, width, orientation(&trailer)?)?;
    let maps = video_maps(&model, width, lens_height)?;

    let folder = new_temp_folder()?;
    let result = (|| -> Result<()> {
        let mut command = Command::new(ffmpeg);
        command.args(["-v", "error", "-y", "-i"]).arg(input);
        let planes: [(&str, &[u16]); 4] = [
            ("x0", &maps.x[0]),
            ("y0", &maps.y[0]),
            ("x1", &maps.x[1]),
            ("y1", &maps.y[1]),
        ];
        // Write each plane directly to its file. Thus, the CLI does not keep a second copy of the maps.
        for (name, plane) in planes {
            let mut file = BufWriter::new(File::create(folder.join(name))?);
            for value in plane {
                file.write_all(&value.to_le_bytes())?;
            }
            file.flush()?;
        }
        std::fs::write(folder.join("mask"), &maps.mask)?;
        for (name, format) in [
            ("x0", "gray16le"),
            ("y0", "gray16le"),
            ("x1", "gray16le"),
            ("y1", "gray16le"),
            ("mask", "gray"),
        ] {
            command.args([
                "-f",
                "rawvideo",
                "-pix_fmt",
                format,
                "-s",
                &format!("{}x{}", maps.width, maps.height),
                "-i",
            ]);
            command.arg(folder.join(name));
        }
        command
            .args([
                "-filter_complex",
                FFMPEG_FILTER,
                "-map",
                "[out]",
                "-map",
                "0:a?",
                "-c:v",
                "libx264",
            ])
            .args([
                "-preset",
                preset,
                "-crf",
                &crf.to_string(),
                "-c:a",
                "copy",
                "-movflags",
                "+faststart",
            ])
            .arg(output);
        let status = command.status()?;
        if !status.success() {
            return Err(format!("ffmpeg exited with {status}").into());
        }
        Ok(())
    })();
    std::fs::remove_dir_all(&folder).ok();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use insta360_stitch::math::IDENTITY;
    use insta360_stitch::{stitch_photo, Lens};

    #[test]
    fn rows_give_the_encoder_the_stitched_rows_across_bands() {
        // 128 rows: more than one band of BAND_ROWS.
        let (width, height) = (256, 128);
        let pixels: Vec<u8> = (0..width * height * 3)
            .map(|i| (i * 7 % 251) as u8)
            .collect();
        let lens = Lens {
            rotation: IDENTITY,
            cx: 64.0,
            cy: 64.0,
            radius: 64.0,
        };
        let model = LensModel {
            lenses: [lens, lens],
            coefficients: [0.0, 0.03159, -8.415e-5, 2.201e-6, -1.284e-8],
            half_fov: 100.0,
            level: IDENTITY,
        };
        let source = Image {
            width,
            height,
            channels: 3,
            data: &pixels,
        };
        let expected = stitch_photo(&source, &model).unwrap();
        let rows = Rows::Stitched {
            source,
            model: &model,
            band: RefCell::new((0, Vec::new())),
        };
        // The encoder asks for each row in sequence and then gives the last row again at the bottom edge.
        // The last request goes back to an earlier band.
        let requests = (0..height).chain([height - 1, height - 1, 5]);
        for y in requests {
            let mut buffers: [Vec<u8>; 4] = Default::default();
            rows.fill_buffers(y as u16, &mut buffers);
            let want: Vec<u8> = expected[y * width * 3..][..width * 3]
                .as_chunks::<3>()
                .0
                .iter()
                .map(|p| rgb_to_ycbcr(p[0], p[1], p[2]).0)
                .collect();
            assert_eq!(buffers[0], want, "row {y}");
        }
    }
}
