use std::cell::RefCell;
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::sync::mpsc::{sync_channel, Receiver, RecvError, SendError, SyncSender};

use clap::{Parser, Subcommand, ValueEnum};
use image::{ImageFormat, ImageReader};
use insta360_stitch::{
    check_different_files, check_photo_layout, preview_rgb, read_imu, read_preview, read_trailer,
    stitch_photo_band, uses_exposure_table, write_spherical_mp4_file, Image, Imu, LensModel,
    Stabilization, Stitcher, Trailer, MAX_VIDEO_WIDTH,
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
    /// Stitches a .insv video into an equirectangular MP4 with 360 video metadata. This command needs ffmpeg and ffprobe
    Video {
        input: PathBuf,
        output: PathBuf,
        #[command(flatten)]
        options: VideoOptions,
    },
}

#[derive(clap::Args)]
struct VideoOptions {
    /// The output width in pixels, a multiple of 4 from 16 to 16384. The default is two times the width of one
    /// lens stream
    #[arg(long, value_parser = parse_width)]
    width: Option<usize>,
    /// lock: level horizon and fixed view direction (as Insta360 Studio). follow: level horizon, the view follows
    /// slow turns. off: the orientation of the first frame for all frames
    #[arg(long, value_enum, default_value_t = StabilizationArg::Lock)]
    stabilization: StabilizationArg,
    /// The ffmpeg video encoder, for example libx264, libx265, hevc_vaapi or hevc_qsv
    #[arg(long, default_value = "libx264")]
    encoder: String,
    /// The quality for libx264 and libx265 (lower is better)
    #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u8).range(0..=51))]
    crf: u8,
    /// The speed preset for libx264 and libx265
    #[arg(long, default_value = "medium")]
    preset: String,
    /// One more ffmpeg output option for the encoder. Repeat it for each word, for example
    /// --encoder-arg=-b:v --encoder-arg=80M
    #[arg(long = "encoder-arg", allow_hyphen_values = true)]
    encoder_args: Vec<String>,
    /// The ffmpeg hardware decoder (-hwaccel), for example vaapi, qsv, cuda or d3d11va
    #[arg(long)]
    hwaccel: Option<String>,
    #[arg(long, default_value = "ffmpeg")]
    ffmpeg: String,
    #[arg(long, default_value = "ffprobe")]
    ffprobe: String,
}

#[derive(Clone, Copy, ValueEnum)]
enum StabilizationArg {
    Lock,
    Follow,
    Off,
}

impl From<StabilizationArg> for Stabilization {
    fn from(value: StabilizationArg) -> Self {
        match value {
            StabilizationArg::Lock => Stabilization::Lock,
            StabilizationArg::Follow => Stabilization::Follow,
            StabilizationArg::Off => Stabilization::Off,
        }
    }
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
            options,
        } => video(&input, &output, &options),
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
    let stitcher = Stitcher::new(trailer_of(path)?);
    let trailer = &stitcher.trailer;
    let offset = &trailer.offset;
    let json = serde_json::json!({
        "stitchable": stitcher.unsupported_reason().is_none(),
        "reason": stitcher.unsupported_reason(),
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
    check_different_files(input, output)?;
    let stitcher = Stitcher::new(trailer_of(input)?);
    // A .insp file is a usual JPEG with the trailer at the end.
    let decoded = ImageReader::with_format(BufReader::new(File::open(input)?), ImageFormat::Jpeg)
        .decode()?
        .into_rgb8();
    let (width, height) = (decoded.width() as usize, decoded.height() as usize);
    check_photo_layout(width, height)?;
    let model = stitcher.model(height)?;
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
    check_different_files(input, output)?;
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

/// Makes a new, empty temporary folder with a unique name in `parent`.
/// `create_dir` fails if the path exists, also if it is a symbolic link. Thus, the CLI does not use a folder of a different process.
fn new_temp_folder(parent: &Path) -> Result<PathBuf> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |time| time.as_nanos());
    for attempt in 0..100 {
        // No leading dot: if the CLI stops before the cleanup, the user can see the folder and delete it.
        let name = format!("insta360-stitch-{}-{nanos}-{attempt}", std::process::id());
        let folder = parent.join(name);
        match std::fs::create_dir(&folder) {
            Ok(()) => return Ok(folder),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Err("cannot make a temporary folder".into())
}

/// Checks `--width`. A valid width also gives a valid lens size: an even number of at least 8 (width / 2).
fn parse_width(text: &str) -> std::result::Result<usize, String> {
    let width: usize = text
        .parse()
        .map_err(|_| format!("{text} is not a number"))?;
    if width < 16 || !width.is_multiple_of(4) || width > MAX_VIDEO_WIDTH {
        return Err(format!(
            "the width must be a multiple of 4 from 16 to {MAX_VIDEO_WIDTH}, not {width}"
        ));
    }
    Ok(width)
}

/// The lens streams of a .insv file, from ffprobe.
#[derive(Debug, PartialEq)]
struct Probe {
    lens_size: usize,
    /// The frame rate as ffprobe gives it (for example 60000/1001), and as a number.
    rate: String,
    fps: f64,
    frame_count: usize,
    /// The colour tags of the source, as ffmpeg output options. A raw pipe does not keep them.
    colour_args: Vec<String>,
}

/// Reads the size, the frame rate, the frame count and the colour tags of the lens streams.
/// A .insv file that the library can stitch has two square video streams, one for each lens.
fn probe(ffprobe: &str, input: &Path) -> Result<Probe> {
    let out = Command::new(ffprobe)
        .args(["-v", "error", "-select_streams", "v", "-show_entries"])
        .arg("stream=width,height,r_frame_rate,nb_frames,color_space,color_primaries,color_transfer,color_range")
        .args(["-of", "compact=p=0"])
        .arg(input)
        .output()?;
    if !out.status.success() {
        return Err(format!(
            "ffprobe failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )
        .into());
    }
    parse_probe(&String::from_utf8_lossy(&out.stdout))
}

/// Parses the ffprobe lines `key=value|key=value...`, one for each video stream.
fn parse_probe(text: &str) -> Result<Probe> {
    let streams: Vec<std::collections::HashMap<&str, &str>> = text
        .lines()
        .filter(|line| line.contains("width="))
        .map(|line| {
            line.trim()
                .split('|')
                .filter_map(|kv| kv.split_once('='))
                .collect()
        })
        .collect();
    let [first, second, ..] = streams.as_slice() else {
        return Err("expected one video stream per lens".into());
    };
    let number = |s: &std::collections::HashMap<&str, &str>, key: &str| -> Option<usize> {
        s.get(key).and_then(|v| v.parse().ok())
    };
    let size = |s| -> Result<(usize, usize)> {
        Ok((
            number(s, "width").ok_or("no stream width")?,
            number(s, "height").ok_or("no stream height")?,
        ))
    };
    let (width, height) = size(first)?;
    let other = size(second)?;
    if other != (width, height) || width != height {
        return Err(format!(
            "unsupported lens streams {width}x{height} and {}x{}",
            other.0, other.1
        )
        .into());
    }
    let rate = first.get("r_frame_rate").copied().unwrap_or("").to_string();
    let fps = match rate.split_once('/') {
        Some((n, d)) => n.parse::<f64>()? / d.parse::<f64>()?,
        None => rate.parse()?,
    };
    if !fps.is_finite() || fps <= 0.0 {
        return Err(format!("invalid frame rate {rate}").into());
    }
    // The two streams can differ by a frame. The CLI stitches the frames that both streams have.
    let frame_count = match (number(first, "nb_frames"), number(second, "nb_frames")) {
        (Some(a), Some(b)) => a.min(b),
        _ => return Err("ffprobe gives no frame count for the lens streams".into()),
    };
    let mut colour_args = Vec::new();
    for (key, option) in [
        ("color_space", "-colorspace"),
        ("color_primaries", "-color_primaries"),
        ("color_transfer", "-color_trc"),
        ("color_range", "-color_range"),
    ] {
        if let Some(value) = first.get(key).filter(|v| !v.is_empty() && **v != "unknown") {
            colour_args.extend([option.to_string(), value.to_string()]);
        }
    }
    Ok(Probe {
        lens_size: width,
        rate,
        fps,
        frame_count,
        colour_args,
    })
}

/// The ffmpeg arguments of the encoder: the codec, then the quality options for libx264 and libx265,
/// then the options of the user.
fn encoder_args(options: &VideoOptions) -> Vec<String> {
    let mut args = vec!["-c:v".to_string(), options.encoder.clone()];
    if matches!(options.encoder.as_str(), "libx264" | "libx265") {
        args.extend([
            "-preset".into(),
            options.preset.clone(),
            "-crf".into(),
            options.crf.to_string(),
        ]);
        args.extend(["-pix_fmt".into(), "yuv420p".into()]);
    }
    args.extend(options.encoder_args.iter().cloned());
    args
}

/// Reads frames of `bytes` from `source` into buffers from `free`, and sends each full buffer to `full`.
/// It stops at the end of the stream, after `limit` frames, or when the receiver stops.
fn read_frames(
    mut source: impl Read,
    bytes: usize,
    limit: usize,
    free: Receiver<Vec<u8>>,
    full: SyncSender<Vec<u8>>,
) -> std::io::Result<usize> {
    let mut count = 0;
    while count < limit {
        let Ok(mut buffer) = free.recv() else { break };
        buffer.resize(bytes, 0);
        match source.read_exact(&mut buffer) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(error) => return Err(error),
        }
        if full.send(buffer).is_err() {
            break;
        }
        count += 1;
    }
    Ok(count)
}

/// Reads the stacked lens frames of `in_bytes` from `decoded`, stitches frame n with `stitch(n, frame, out)`
/// and writes each output of `out_bytes` to `encoded`. It stops after `limit` frames or at the end of
/// `decoded`, and gives the number of frames. Two buffers on each side: one in use, one in transfer.
/// Thus, decode, stitch and encode overlap.
fn pipeline(
    decoded: impl Read + Send,
    mut encoded: impl Write + Send,
    (in_bytes, out_bytes): (usize, usize),
    limit: usize,
    stitch: impl Fn(usize, &[u8], &mut [u8]) -> Result<()>,
) -> Result<usize> {
    let (free_in_tx, free_in) = sync_channel(2);
    let (full_tx, full) = sync_channel(2);
    let (free_out_tx, free_out) = sync_channel(2);
    let (done_tx, done) = sync_channel::<Vec<u8>>(2);
    for _ in 0..2 {
        free_in_tx.send(Vec::new())?;
        free_out_tx.send(vec![0u8; out_bytes])?;
    }
    let started = std::time::Instant::now();
    std::thread::scope(|scope| -> Result<usize> {
        // Moved into the scope: an early return drops it, thus a reader that waits for a free buffer stops.
        let free_in_tx = free_in_tx;
        let reader = scope.spawn(move || read_frames(decoded, in_bytes, limit, free_in, full_tx));
        let writer = scope.spawn(move || -> std::io::Result<()> {
            for buffer in done {
                encoded.write_all(&buffer)?;
                if free_out_tx.send(buffer).is_err() {
                    break;
                }
            }
            encoded.flush()
        });
        let mut index = 0;
        for frame in full {
            let mut out = free_out.recv()?;
            stitch(index, &frame, &mut out)?;
            free_in_tx.send(frame).ok();
            done_tx.send(out)?;
            index += 1;
            if index % 30 == 0 {
                let fps = index as f64 / started.elapsed().as_secs_f64();
                eprint!("\rframe {index}/{limit} ({fps:.1} fps)");
            }
        }
        drop(done_tx);
        let read = reader.join().map_err(|_| "the decoder thread panicked")??;
        writer.join().map_err(|_| "the encoder thread panicked")??;
        Ok(read.min(index))
    })
}

/// The encoder: raw stitched frames from the pipe (input 0), the audio and the metadata from the .insv
/// (input 1).
fn encoder_command(
    options: &VideoOptions,
    probe: &Probe,
    (width, height): (usize, usize),
    input: &Path,
    stitched: &Path,
) -> Command {
    let mut command = Command::new(&options.ffmpeg);
    command
        .args(["-v", "error", "-y", "-f", "rawvideo", "-pix_fmt", "yuv420p"])
        .args([
            "-s",
            &format!("{width}x{height}"),
            "-framerate",
            &probe.rate,
        ])
        // The colour tags of the source on the pipe input. Thus, the output gets them, and ffmpeg does not
        // convert the frames: with the tags on the output only, ffmpeg changes full range to limited range.
        .args(&probe.colour_args)
        .args(["-i", "pipe:0", "-i"])
        .arg(input)
        .args(["-map", "0:v:0", "-map", "1:a?", "-c:a", "copy"])
        // The global metadata (for example creation_time) of the .insv. ffmpeg copies it from input 0 only.
        .args(["-map_metadata", "1"])
        .args(encoder_args(options))
        // ffmpeg writes moov at the end. finish_video moves it to the start and adds the 360 video metadata.
        .arg(stitched);
    command
}

fn video(input: &Path, output: &Path, options: &VideoOptions) -> Result<()> {
    check_different_files(input, output)?;
    let mut reader = BufReader::new(File::open(input)?);
    let stitcher = Stitcher::new(read_trailer(&mut reader)?);
    let probe = probe(&options.ffprobe, input)?;
    // For an output that is narrower than two lens streams, ffmpeg scales the lenses down first (area filter).
    // Thus, the stitch does not alias, and the pipe moves less data.
    let width = options.width.unwrap_or(2 * probe.lens_size);
    let lens_size = probe.lens_size.min(width / 2);
    let frames = stitcher.frame_stitcher(lens_size, Some(width))?;
    let mode = Stabilization::from(options.stabilization);
    // Off uses the IMU window of the trailer. Lock and follow need the full stream.
    let imu = match mode {
        Stabilization::Off => Imu::default(),
        _ => read_imu(&mut reader, &stitcher.trailer)?,
    };
    let rotations = match stitcher.frame_rotations(&imu, probe.frame_count, probe.fps, mode) {
        Err(insta360_stitch::Error::Unsupported(why)) if mode != Stabilization::Off => {
            eprintln!("insta360-stitch: no stabilization: {why}");
            stitcher.frame_rotations(&imu, probe.frame_count, probe.fps, Stabilization::Off)?
        }
        rotations => rotations?,
    };
    if mode != Stabilization::Off && !uses_exposure_table(&stitcher.trailer, probe.frame_count) {
        eprintln!("insta360-stitch: the exposure table does not have all frames, the frame times use the frame rate");
    }
    drop(imu);

    // The work folder is next to the output, not in the system temporary folder.
    // That folder can be in memory (tmpfs), and a long 8K encode can be larger than the memory.
    // Also, the stitched file can then become the output with a rename.
    let parent = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let folder = new_temp_folder(parent)?;
    let stitched = folder.join("stitched.mp4");
    let result = (|| -> Result<()> {
        // Decode: lens 0 is the second video stream. ffmpeg stacks the two lenses into one yuv420p frame.
        let mut decode = Command::new(&options.ffmpeg);
        decode.args(["-v", "error", "-nostdin"]);
        if let Some(hwaccel) = &options.hwaccel {
            decode.args(["-hwaccel", hwaccel]);
        }
        let mut decoder = decode
            .arg("-i")
            .arg(input)
            .arg("-filter_complex")
            .arg(if lens_size < probe.lens_size {
                format!("[0:v:1][0:v:0]vstack=inputs=2,scale={lens_size}:{}:flags=area,format=yuv420p[v]", 2 * lens_size)
            } else {
                "[0:v:1][0:v:0]vstack=inputs=2,format=yuv420p[v]".to_string()
            })
            .args([
                // passthrough: no frame is dropped or repeated, thus frame n keeps the rotation of frame n.
                "-map",
                "[v]",
                "-fps_mode",
                "passthrough",
                "-f",
                "rawvideo",
                "-pix_fmt",
                "yuv420p",
                "pipe:1",
            ])
            .stdout(Stdio::piped())
            .spawn()?;
        let mut encoder = encoder_command(
            options,
            &probe,
            (frames.width(), frames.height()),
            input,
            &stitched,
        )
        .stdin(Stdio::piped())
        .spawn()?;
        let decoded = decoder.stdout.take().ok_or("no decoder output")?;
        let encoded = BufWriter::new(encoder.stdin.take().ok_or("no encoder input")?);

        let started = std::time::Instant::now();
        let count = pipeline(
            decoded,
            encoded,
            (frames.input_frame_bytes(), frames.output_frame_bytes()),
            probe.frame_count,
            |index, frame, out| Ok(frames.stitch_yuv420(&rotations[index], frame, out)?),
        );
        // Stop the decoder if the stitch stopped early. Its exit status is then not an error.
        let count = match count {
            Ok(count) => count,
            Err(error) => {
                decoder.kill().ok();
                decoder.wait().ok();
                // An encoder that stopped is the cause, not the closed pipe that the stitch then sees.
                let status = encoder.wait()?;
                if !status.success() {
                    return Err(format!("the ffmpeg encoder exited with {status}").into());
                }
                if error.is::<RecvError>() || error.is::<SendError<Vec<u8>>>() {
                    return Err("the ffmpeg encoder stopped before the last frame".into());
                }
                return Err(error);
            }
        };
        let decoded_ok = if count < probe.frame_count {
            decoder.wait()?.success()
        } else {
            decoder.kill().ok();
            decoder.wait().ok();
            true
        };
        let encoder_status = encoder.wait()?;
        eprintln!(
            "\rframe {count}/{} in {:.1} s",
            probe.frame_count,
            started.elapsed().as_secs_f64()
        );
        if !decoded_ok {
            return Err("the ffmpeg decoder failed".into());
        }
        if !encoder_status.success() {
            return Err(format!("the ffmpeg encoder exited with {encoder_status}").into());
        }
        if count == 0 {
            return Err("the decoder gave no frames".into());
        }
        if count < probe.frame_count {
            eprintln!(
                "insta360-stitch: the decoder gave {count} of {} frames",
                probe.frame_count
            );
        }
        finish_video(&stitched, output)
    })();
    std::fs::remove_dir_all(&folder).ok();
    result
}

/// Adds the 360 video metadata to the ffmpeg output `stitched` and writes it to `output`.
/// If that fails, the CLI keeps the encode: it moves `stitched` to `output` without the metadata.
/// Without the metadata, the video is correct, but players show it flat.
fn finish_video(stitched: &Path, output: &Path) -> Result<()> {
    if let Err(error) = write_spherical_mp4_file(stitched, output) {
        eprintln!("insta360-stitch: the output has no 360 video metadata: {error}");
        std::fs::rename(stitched, output)?;
    }
    Ok(())
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

    /// A new, empty folder for one test.
    fn test_folder(name: &str) -> PathBuf {
        let folder =
            std::env::temp_dir().join(format!("insta360-cli-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&folder).ok();
        std::fs::create_dir_all(&folder).unwrap();
        folder
    }

    fn make_box(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        [&((8 + body.len()) as u32).to_be_bytes()[..], kind, body].concat()
    }

    /// A minimal MP4 file as ffmpeg writes it without faststart: ftyp, mdat, then moov with one video track.
    fn minimal_mp4() -> Vec<u8> {
        let full = |kind: &[u8; 4], body: &[u8]| make_box(kind, &[&[0u8; 4][..], body].concat());
        let ftyp = make_box(b"ftyp", b"isom\0\0\x02\0isomiso2");
        let mdat = make_box(b"mdat", b"VIDEO-ONE");
        let entry = make_box(b"avc1", &[0u8; 78]);
        let stsd = full(b"stsd", &[&1u32.to_be_bytes()[..], &entry].concat());
        let offset = (ftyp.len() as u32 + 8).to_be_bytes();
        let stco = full(b"stco", &[&1u32.to_be_bytes()[..], &offset].concat());
        let hdlr = full(b"hdlr", &[&[0u8; 4][..], b"vide", &[0u8; 13]].concat());
        let minf = make_box(b"minf", &make_box(b"stbl", &[stsd, stco].concat()));
        let trak = make_box(b"trak", &make_box(b"mdia", &[hdlr, minf].concat()));
        [ftyp, mdat, make_box(b"moov", &trak)].concat()
    }

    #[test]
    fn finish_video_keeps_the_encode_when_the_metadata_step_fails() {
        let folder = test_folder("finish");
        let (stitched, output) = (folder.join("stitched.mp4"), folder.join("out.mp4"));
        std::fs::write(&stitched, minimal_mp4()).unwrap();
        finish_video(&stitched, &output).unwrap();
        let out = std::fs::read(&output).unwrap();
        assert!(out.windows(4).any(|w| w == b"sv3d"));

        // The metadata step refuses this file. The encode must become the output, not disappear.
        std::fs::write(&stitched, b"an encode that is not a usual MP4 file").unwrap();
        finish_video(&stitched, &output).unwrap();
        assert_eq!(
            std::fs::read(&output).unwrap(),
            b"an encode that is not a usual MP4 file"
        );
        assert!(!stitched.exists());
        std::fs::remove_dir_all(&folder).unwrap();
    }

    #[test]
    fn each_command_refuses_to_write_over_its_input() {
        let folder = test_folder("same");
        let file = folder.join("VID_1.insv");
        std::fs::write(&file, b"original footage").unwrap();
        let results = [
            photo(&file, &file, 95),
            preview(&file, &file),
            video(&file, &file, &test_video_options()),
        ];
        for result in results {
            let error = result.unwrap_err().to_string();
            assert!(error.contains("different file"), "{error}");
        }
        assert_eq!(std::fs::read(&file).unwrap(), b"original footage");
        std::fs::remove_dir_all(&folder).unwrap();
    }

    fn test_video_options() -> VideoOptions {
        VideoOptions {
            width: None,
            stabilization: StabilizationArg::Lock,
            encoder: "libx264".into(),
            crf: 20,
            preset: "medium".into(),
            encoder_args: vec!["-tune".into(), "film".into()],
            hwaccel: None,
            ffmpeg: "ffmpeg".into(),
            ffprobe: "ffprobe".into(),
        }
    }

    #[test]
    fn probe_reads_square_lens_streams_the_smaller_frame_count_and_the_colour_tags() {
        let line = |frames: &str| {
            format!("width=3840|height=3840|r_frame_rate=24/1|nb_frames={frames}|color_range=tv|color_space=bt709|color_transfer=bt709|color_primaries=unknown\n")
        };
        let probe = parse_probe(&(line("80") + &line("81"))).unwrap();
        assert_eq!(
            probe,
            Probe {
                lens_size: 3840,
                rate: "24/1".into(),
                fps: 24.0,
                frame_count: 80,
                colour_args: [
                    "-colorspace",
                    "bt709",
                    "-color_trc",
                    "bt709",
                    "-color_range",
                    "tv"
                ]
                .map(String::from)
                .to_vec(),
            }
        );
        let ntsc = "width=2880|height=2880|r_frame_rate=60000/1001|nb_frames=233\n".repeat(2);
        assert!((parse_probe(&ntsc).unwrap().fps - 59.94).abs() < 0.01);
        for bad in [
            line("80"),
            "width=3840|height=1920|r_frame_rate=24/1|nb_frames=80\n".repeat(2),
            "width=64|height=64|r_frame_rate=0/1|nb_frames=5\n".repeat(2),
            "width=64|height=64|r_frame_rate=24/1|nb_frames=N/A\n".repeat(2),
        ] {
            assert!(parse_probe(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn encoder_args_add_quality_only_for_x264_and_x265() {
        let mut options = test_video_options();
        assert_eq!(
            encoder_args(&options),
            [
                "-c:v", "libx264", "-preset", "medium", "-crf", "20", "-pix_fmt", "yuv420p",
                "-tune", "film"
            ]
        );
        options.encoder = "hevc_vaapi".into();
        options.encoder_args = vec!["-vf".into(), "format=nv12,hwupload".into()];
        assert_eq!(
            encoder_args(&options),
            ["-c:v", "hevc_vaapi", "-vf", "format=nv12,hwupload"]
        );
    }

    #[test]
    fn the_encoder_copies_the_metadata_and_the_colour_tags_and_keeps_all_frames() {
        let line = "width=64|height=64|r_frame_rate=24/1|nb_frames=5|color_range=pc\n";
        let probe = parse_probe(&line.repeat(2)).unwrap();
        let command = encoder_command(
            &test_video_options(),
            &probe,
            (128, 64),
            Path::new("in.insv"),
            Path::new("out.mp4"),
        );
        let args: Vec<_> = command.get_args().map(|a| a.to_string_lossy()).collect();
        let at = |value: &str| args.iter().position(|a| a == value).unwrap();
        // The metadata comes from the .insv, the second input. It is an output option: after the inputs.
        assert_eq!(args[at("in.insv") - 1], "-i");
        assert_eq!(args[at("-map_metadata") + 1], "1");
        assert!(at("-map_metadata") > at("in.insv"), "{args:?}");
        // The colour tags are input options of the pipe: ffmpeg then does not convert the frames.
        assert!(at("-color_range") < at("pipe:0"), "{args:?}");
        assert_eq!(args.iter().filter(|a| *a == "-color_range").count(), 1);
        // The audio of an X5 clip ends before the last frame: -shortest would remove the last frames.
        assert!(!args.iter().any(|a| a == "-shortest"), "{args:?}");
    }

    #[test]
    fn width_must_be_a_multiple_of_4_from_16() {
        for good in ["16", "3840", "16384"] {
            assert!(parse_width(good).is_ok(), "{good}");
        }
        // 3002 gives the odd lens size 1501, and 12 gives 6: the error must name the width.
        for bad in ["3002", "12", "0", "16388", "wide"] {
            assert!(parse_width(bad).is_err(), "{bad}");
        }
    }

    /// An encoder pipe that is slower than the stitch and stops after `0` writes, as an ffmpeg that exits.
    struct StoppingWriter(usize);

    impl Write for StoppingWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            std::thread::sleep(std::time::Duration::from_millis(20));
            if self.0 == 0 {
                return Err(std::io::ErrorKind::BrokenPipe.into());
            }
            self.0 -= 1;
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Runs `run` in a thread and fails if it does not end in 10 s: a pipeline that waits without end.
    fn in_time<T: Send + 'static>(run: impl FnOnce() -> T + Send + 'static) -> T {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || tx.send(run()).ok());
        rx.recv_timeout(std::time::Duration::from_secs(10))
            .expect("the pipeline waits without end")
    }

    #[test]
    fn pipeline_stitches_each_frame_in_order_and_stops_when_the_encoder_stops() {
        let (count, output) = in_time(|| {
            let input: Vec<u8> = (0..5u8).flat_map(|i| [i; 4]).collect();
            let mut output = Vec::new();
            let count = pipeline(&input[..], &mut output, (4, 2), 10, |index, frame, out| {
                out.copy_from_slice(&[frame[0], index as u8]);
                Ok(())
            });
            // The error type is not Send: give its text.
            (count.map_err(|e| e.to_string()), output)
        });
        assert_eq!(count, Ok(5));
        assert_eq!(output, [0, 0, 1, 1, 2, 2, 3, 3, 4, 4]);

        // The encoder or the stitch stops while the decoder still has frames: the pipeline must give an error,
        // not wait. The writer is slower than the stitch, thus the stitch waits for an output buffer.
        let stops = |writes: usize, stitch_error_at: usize| {
            in_time(move || {
                pipeline(
                    std::io::repeat(1),
                    StoppingWriter(writes),
                    (16, 16),
                    1000,
                    |index, _, _| match index == stitch_error_at {
                        true => Err("stitch error".into()),
                        false => Ok(()),
                    },
                )
                .map_err(|e| e.to_string())
            })
        };
        assert!(stops(3, usize::MAX).is_err());
        assert_eq!(stops(usize::MAX, 5), Err("stitch error".into()));
    }

    #[test]
    fn read_frames_gives_full_frames_and_stops_at_the_end_or_the_limit() {
        let data: Vec<u8> = (0..10).collect();
        let run = |limit: usize| {
            let (free_tx, free) = sync_channel(4);
            let (full_tx, full) = sync_channel(4);
            for _ in 0..4 {
                free_tx.send(Vec::new()).unwrap();
            }
            let n = read_frames(&data[..], 4, limit, free, full_tx).unwrap();
            (n, full.iter().collect::<Vec<_>>())
        };
        // 10 bytes in frames of 4: two full frames, the rest is not a frame.
        assert_eq!(run(10), (2, vec![vec![0, 1, 2, 3], vec![4, 5, 6, 7]]));
        assert_eq!(run(1), (1, vec![vec![0, 1, 2, 3]]));
    }
}
