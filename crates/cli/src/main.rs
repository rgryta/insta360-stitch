use std::fs::File;
use std::io::{BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use clap::{Parser, Subcommand};
use image::codecs::jpeg::JpegEncoder;
use image::{ExtendedColorType, ImageFormat, ImageReader};
use insta360_stitch::{
    check_stitchable, orientation, preview_rgb, read_preview, read_trailer, stitch_photo,
    video_maps, Image, LensModel, Trailer, FFMPEG_FILTER,
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Stitch Insta360 dual-fisheye photos (.insp) and videos (.insv) into equirectangular panoramas.
#[derive(Parser)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Print what the file's trailer says, as JSON
    Info { file: PathBuf },
    /// Stitch a .insp photo into an equirectangular JPEG
    Photo {
        input: PathBuf,
        output: PathBuf,
        #[arg(long, default_value_t = 95)]
        quality: u8,
    },
    /// Write the camera's own small stitched preview as a JPEG
    Preview { input: PathBuf, output: PathBuf },
    /// Stitch a .insv video into an equirectangular MP4 (needs ffmpeg and ffprobe)
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

/// A JPEG with an XMP GPano packet, so other viewers open it as a 360 panorama.
fn write_panorama_jpeg(
    output: &Path,
    rgb: &[u8],
    width: usize,
    height: usize,
    quality: u8,
) -> Result<()> {
    let mut jpeg = Vec::new();
    JpegEncoder::new_with_quality(&mut jpeg, quality).encode(
        rgb,
        width as u32,
        height as u32,
        ExtendedColorType::Rgb8,
    )?;
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
    out.write_all(&jpeg[..2])?; // SOI
    out.write_all(&[0xff, 0xe1])?;
    out.write_all(&((xmp.len() + 2) as u16).to_be_bytes())?;
    out.write_all(xmp.as_bytes())?;
    out.write_all(&jpeg[2..])?;
    out.flush()?;
    Ok(())
}

fn photo(input: &Path, output: &Path, quality: u8) -> Result<()> {
    let trailer = trailer_of(input)?;
    // .insp is a plain JPEG with the trailer appended
    let decoded = ImageReader::with_format(BufReader::new(File::open(input)?), ImageFormat::Jpeg)
        .decode()?
        .into_rgb8();
    let (width, height) = (decoded.width() as usize, decoded.height() as usize);
    let model = LensModel::new(&trailer, height, orientation(&trailer)?)?;
    let stitched = stitch_photo(
        &Image {
            width,
            height,
            channels: 3,
            data: decoded.as_raw(),
        },
        &model,
    )?;
    drop(decoded);
    write_panorama_jpeg(output, &stitched, width, height, quality)
}

fn preview(input: &Path, output: &Path) -> Result<()> {
    let mut reader = BufReader::new(File::open(input)?);
    let trailer = read_trailer(&mut reader)?;
    let preview = read_preview(&mut reader, &trailer)?;
    let rgb = preview_rgb(&preview);
    write_panorama_jpeg(
        output,
        &rgb,
        preview.width as usize,
        preview.height as usize,
        92,
    )
}

/// The first video stream's size; a stitchable .insv has two, one per lens.
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
    // shortcut: levelled once, at the first frame; per-frame levelling needs per-frame maps
    let model = LensModel::new(&trailer, width, orientation(&trailer)?)?;
    let maps = video_maps(&model, width, lens_height)?;

    let folder = std::env::temp_dir().join(format!("insta360-stitch-{}", std::process::id()));
    std::fs::create_dir_all(&folder)?;
    let result = (|| -> Result<()> {
        let planes: [(&str, Vec<u8>); 5] = [
            (
                "x0",
                maps.x[0].iter().flat_map(|v| v.to_le_bytes()).collect(),
            ),
            (
                "y0",
                maps.y[0].iter().flat_map(|v| v.to_le_bytes()).collect(),
            ),
            (
                "x1",
                maps.x[1].iter().flat_map(|v| v.to_le_bytes()).collect(),
            ),
            (
                "y1",
                maps.y[1].iter().flat_map(|v| v.to_le_bytes()).collect(),
            ),
            ("mask", maps.mask.clone()),
        ];
        let mut command = Command::new(ffmpeg);
        command.args(["-v", "error", "-y", "-i"]).arg(input);
        for (name, bytes) in &planes {
            std::fs::write(folder.join(name), bytes)?;
            let format = if *name == "mask" { "gray" } else { "gray16le" };
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
        drop(planes);
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
