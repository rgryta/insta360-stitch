//! This module reads the trailer that Insta360 adds to the end of .insp (JPEG) and .insv (MP4) files.
//!
//! The trailer is a sequence of records. Each record has its data first, then a `[u16 id][u32 length]` header.
//! The reader starts at the 78-byte tail at the end of the file and moves back one record at a time.
//! The tail contains the total length of the trailer (`u32` at offset 38) and a 32-byte magic value (at offset 46).
//! In a video file, a directory record (id 0) replaces this sequence.
//! Each directory entry is `[u16 id][u32 size][u32 offset]`.
//!
//! The reader reads only the data that it needs: the headers, the metadata, the exposure table of a video and the
//! IMU samples until a time limit.
//! It reads the preview and the full IMU stream only if you ask for them.
//! It does not read the full trailer, because the trailer becomes larger when the video becomes longer.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};

use crate::error::{unsupported, Error, Result};

/// The number of bytes at the end of each Insta360 file that give the position of the trailer.
pub const TAIL_LENGTH: usize = 78;
const MAGIC: &[u8; 32] = b"8db42d694ccc418790edff439fe026bf";

const RECORD_DIRECTORY: u16 = 0x0;
const RECORD_EXTRA_METADATA: u16 = 0x101;
const RECORD_PREVIEW: u16 = 0x200;
const RECORD_IMU: u16 = 0x300;
const RECORD_EXPOSURE_LENS1: u16 = 0xc00;
const RECORD_EXPOSURE: u16 = 0x400;
/// One exposure entry: `[u64 exposure start, us on the IMU clock][f64 exposure duration, s]`.
const EXPOSURE_BYTES: u64 = 16;
/// The exposure table of one hour at 120 fps has 432 000 entries (6.9 MB).
const MAX_EXPOSURE_BYTES: u64 = 16 << 20;
/// `read_imu` averages the IMU samples down to this rate. The gyroscope changes slowly compared with it.
const IMU_FULL_MAX_RATE_HZ: f64 = 250.0;
const IMU_SAMPLE_BYTES: usize = 20;
const PREVIEW_HEADER_BYTES: u64 = 40;
/// The reader decodes IMU samples until this time after the first frame (µs).
/// The 0.02 Hz low-pass filter gives no weight to later samples.
/// The IMU stream has approximately 1000 samples per second for the full length of a video.
const IMU_WINDOW_US: f64 = 120e6;
const IMU_READ_CHUNK: usize = 4096;
/// Real trailers have fewer than 20 records. A crafted file must not make the reader walk millions of them.
const MAX_RECORDS: usize = 256;
/// Real metadata records have some kB. The reader refuses larger records instead of a large allocation.
const MAX_METADATA_BYTES: u64 = 1 << 20;
const MAX_DIRECTORY_BYTES: u64 = 64 << 10;
/// The first frame comes some seconds after the IMU start. A later value is not real.
/// The reader limits it, thus it does not decode the full IMU stream of a long video.
const MAX_FIRST_FRAME_DELAY_US: f64 = 600e6;

/// The raw data from the motion sensor.
/// Each sample has a time (µs) and 6 raw `u16` values: accelerometer x, y, z, then gyroscope x, y, z.
/// The value 32768 is zero.
#[derive(Debug, Clone, Default)]
pub struct Imu {
    pub ts: Vec<f64>,
    pub raw: Vec<u16>,
}

impl Imu {
    pub fn len(&self) -> usize {
        self.ts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ts.is_empty()
    }
}

/// One video frame in the exposure table of the camera.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Exposure {
    /// The end of the exposure on the IMU clock (µs).
    pub timestamp_us: f64,
    /// The exposure time (s).
    pub duration_s: f64,
}

/// A small equirectangular image that the camera stitched. The format is NV12 (BT.601, limited range).
#[derive(Debug, Clone)]
pub struct Preview {
    pub width: u32,
    pub height: u32,
    pub nv12: Vec<u8>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct PreviewLocation {
    pub width: u32,
    pub height: u32,
    pub offset: u64,
}

/// The data in a trailer that the stitcher needs.
#[derive(Debug, Clone)]
pub struct Trailer {
    /// The calibration: count, two sets of [radius cx cy rx ry rz], calibration width, calibration height, lens word.
    pub offset: [f64; 16],
    /// The state of the lens guard or accessory: 0 = none, 4 = standard guard, 5 = premium guard.
    pub offset_state: u64,
    /// The time on the camera clock when the camera made the file, as `YYYYMMDDhhmmss` digits.
    pub local_time: Option<String>,
    /// The time when the camera exposed the first frame, on the IMU clock (µs).
    pub first_frame_us: Option<f64>,
    /// The full-scale range of the accelerometer (g).
    pub acc_range: f64,
    /// The full-scale range of the gyroscope (degrees per second).
    pub gyro_range: f64,
    pub imu: Option<Imu>,
    /// The exposure table of a video (lens 0), one entry for each recorded frame. The camera records some frames
    /// before and after the frames of the MP4 file, thus the table can be longer than the video.
    pub exposures: Option<Vec<Exposure>>,
    /// The exposure table of lens 1 (record 0xc00, `EXPOTIME_VIN1`), if the file has one.
    pub exposures_lens1: Option<Vec<Exposure>>,
    /// The time from the first to the last row of a frame (rolling shutter, ExtraMetadata field 25
    /// `rolling_shutter_time`, ms). 0 if the file does not give it.
    pub rolling_shutter_ms: f64,
    /// The time from an exposure to its IMU samples (ExtraMetadata field 28 `gyro_timestamp`, ms). Insta360 adds
    /// it to the frame times.
    pub gyro_delay_ms: f64,
    pub(crate) preview: Option<PreviewLocation>,
    /// The position and the length of the full IMU record.
    pub(crate) imu_location: Option<(u64, u64)>,
}

impl Trailer {
    /// Gives the width and height of the preview. Gives `None` if the trailer has no preview.
    pub fn preview_size(&self) -> Option<(u32, u32)> {
        self.preview.map(|p| (p.width, p.height))
    }
}

/// Gives the total length of the trailer from the last 78 bytes of a file.
/// This function checks the length before the reader reads that number of bytes.
pub fn trailer_length(tail: &[u8], file_size: u64) -> Result<u64> {
    if tail.len() != TAIL_LENGTH || &tail[46..] != MAGIC {
        return unsupported("no Insta360 trailer");
    }
    let length = u64::from(u32::from_le_bytes(tail[38..42].try_into().unwrap()));
    if length < TAIL_LENGTH as u64 || length > file_size {
        return unsupported("no Insta360 trailer");
    }
    Ok(length)
}

fn read_at<R: Read + Seek>(reader: &mut R, position: u64, length: usize) -> Result<Vec<u8>> {
    reader.seek(SeekFrom::Start(position))?;
    let mut buffer = vec![0; length];
    // A short read is an I/O error, and a retry can work. It does not mean "no records"
    reader.read_exact(&mut buffer).map_err(Error::Io)?;
    Ok(buffer)
}

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

/// Reads a trailer from an Insta360 file, or from a different seekable source that contains a trailer.
pub fn read_trailer<R: Read + Seek>(reader: &mut R) -> Result<Trailer> {
    let size = reader.seek(SeekFrom::End(0))?;
    if size < TAIL_LENGTH as u64 {
        return unsupported("no Insta360 trailer");
    }
    let tail = read_at(reader, size - TAIL_LENGTH as u64, TAIL_LENGTH)?;
    let length = trailer_length(&tail, size)?;
    let start = size - length;
    let records = index_records(reader, start, length)?;

    let Some(&(metadata_at, metadata_len)) = records.get(&RECORD_EXTRA_METADATA) else {
        return unsupported("no ExtraMetadata record in trailer");
    };
    if metadata_len > MAX_METADATA_BYTES {
        return unsupported("ExtraMetadata record is too large");
    }
    let metadata = read_at(reader, metadata_at, metadata_len as usize)?;
    let mut trailer = parse_metadata(&metadata)?;

    if let Some(&(imu_at, imu_len)) = records.get(&RECORD_IMU) {
        trailer.imu = read_imu_window(reader, imu_at, imu_len, trailer.first_frame_us)?;
        trailer.imu_location = Some((imu_at, imu_len));
    }

    trailer.exposures = read_exposures(reader, records.get(&RECORD_EXPOSURE))?;
    trailer.exposures_lens1 = read_exposures(reader, records.get(&RECORD_EXPOSURE_LENS1))?;

    if let Some(&(preview_at, preview_len)) = records.get(&RECORD_PREVIEW) {
        if preview_len >= PREVIEW_HEADER_BYTES {
            // The header is [u32 1][u32 record length][u32 1][u32 0][u32 width][u32 height].
            // Zero bytes fill it to 40 bytes. The NV12 data follows the header.
            let header = read_at(reader, preview_at, PREVIEW_HEADER_BYTES as usize)?;
            let (width, height) = (u32_at(&header, 16), u32_at(&header, 20));
            let pixels = u64::from(width) * u64::from(height);
            // NV12 keeps one chroma pair for each 2x2 block. Thus, the width and the height must be even.
            if u32_at(&header, 0) == 1
                && pixels > 0
                && width % 2 == 0
                && height % 2 == 0
                && pixels
                    .checked_mul(3)
                    .map(|bytes| PREVIEW_HEADER_BYTES + bytes / 2)
                    == Some(preview_len)
            {
                trailer.preview = Some(PreviewLocation {
                    width,
                    height,
                    offset: preview_at + PREVIEW_HEADER_BYTES,
                });
            }
        }
    }
    Ok(trailer)
}

/// Reads a trailer that is in memory. The bytes start at the trailer and continue to the end of the file.
pub fn parse_trailer(bytes: &[u8]) -> Result<Trailer> {
    read_trailer(&mut std::io::Cursor::new(bytes))
}

/// Reads the preview from the trailer. Use the same source that you used to read `trailer`.
pub fn read_preview<R: Read + Seek>(reader: &mut R, trailer: &Trailer) -> Result<Preview> {
    let Some(location) = trailer.preview else {
        return unsupported("no preview in trailer");
    };
    let length = (u64::from(location.width) * u64::from(location.height) * 3 / 2) as usize;
    let nv12 = read_at(reader, location.offset, length)?;
    Ok(Preview {
        width: location.width,
        height: location.height,
        nv12,
    })
}

/// Makes a table of record id -> (absolute offset, length). It reads only the headers.
fn index_records<R: Read + Seek>(
    reader: &mut R,
    start: u64,
    length: u64,
) -> Result<HashMap<u16, (u64, u64)>> {
    let mut records = HashMap::new();
    let mut header = length - TAIL_LENGTH as u64;
    let mut walked = 0;
    while header >= 6 {
        walked += 1;
        if walked > MAX_RECORDS {
            return unsupported("too many trailer records");
        }
        let bytes = read_at(reader, start + header, 6)?;
        let (id, record_len) = (u16_at(&bytes, 0), u64::from(u32_at(&bytes, 2)));
        if record_len > header {
            break;
        }
        if id == RECORD_DIRECTORY {
            if record_len > MAX_DIRECTORY_BYTES {
                return unsupported("trailer directory is too large");
            }
            let table = read_at(reader, start + header - record_len, record_len as usize)?;
            for entry in table.as_chunks::<10>().0 {
                let size = u64::from(u32_at(entry, 2));
                let offset = u64::from(u32_at(entry, 6));
                if size > 0 && offset + size + 6 <= length {
                    // The table gives short ids (0x3 for 0x300). The footer of each record gives the full id.
                    let footer = read_at(reader, start + offset + size, 2)?;
                    records.insert(u16_at(&footer, 0), (start + offset, size));
                }
            }
            break;
        }
        records.insert(id, (start + header - record_len, record_len));
        let Some(previous) = header.checked_sub(record_len + 6) else {
            break;
        };
        header = previous;
    }
    Ok(records)
}

/// Reads the full IMU stream of a video, not only the first two minutes that `read_trailer` reads.
/// It averages consecutive samples down to at most 250 Hz. Thus, one hour needs approximately 18 MB.
/// Use the same source that you used to read `trailer`.
pub fn read_imu<R: Read + Seek>(reader: &mut R, trailer: &Trailer) -> Result<Imu> {
    let Some((at, len)) = trailer.imu_location else {
        return unsupported("no IMU record in trailer");
    };
    let total = (len / IMU_SAMPLE_BYTES as u64) as usize;
    if total == 0 {
        return unsupported("no IMU record in trailer");
    }
    let mut imu = Imu::default();
    let mut group: usize = 0; // samples in each average, from the sample rate of the first chunk
    let (mut ts_sum, mut raw_sum, mut in_group) = (0f64, [0u64; 6], 0usize);
    let mut remainder = [0f64; 6];
    let mut index = 0;
    while index < total {
        let count = IMU_READ_CHUNK.min(total - index);
        let chunk = read_at(
            reader,
            at + (index * IMU_SAMPLE_BYTES) as u64,
            count * IMU_SAMPLE_BYTES,
        )?;
        let samples = chunk.as_chunks::<IMU_SAMPLE_BYTES>().0;
        if group == 0 {
            let first = u64::from_le_bytes(samples[0][..8].try_into().unwrap()) as f64;
            let last =
                u64::from_le_bytes(samples[samples.len() - 1][..8].try_into().unwrap()) as f64;
            let rate = (samples.len() - 1) as f64 / ((last - first) / 1e6);
            group = if rate.is_finite() && rate > IMU_FULL_MAX_RATE_HZ {
                // The tolerance keeps exactly 1000 Hz at groups of 4, not 5.
                (rate / IMU_FULL_MAX_RATE_HZ - 1e-3).ceil() as usize
            } else {
                1
            };
        }
        for sample in samples {
            ts_sum += u64::from_le_bytes(sample[..8].try_into().unwrap()) as f64;
            for (k, sum) in raw_sum.iter_mut().enumerate() {
                *sum += u64::from(u16_at(sample, 8 + k * 2));
            }
            in_group += 1;
            if in_group == group {
                push_average(&mut imu, ts_sum, &raw_sum, in_group, &mut remainder);
                (ts_sum, raw_sum, in_group) = (0.0, [0; 6], 0);
            }
        }
        index += count;
    }
    if in_group > 0 {
        push_average(&mut imu, ts_sum, &raw_sum, in_group, &mut remainder);
    }
    Ok(imu)
}

/// Adds the mean of a group. The rounding remainder of each channel goes into the next mean.
/// Thus, the rounding adds no bias, and the integrated gyroscope does not drift because of it.
/// (The bias of the gyroscope itself is larger. The library does not remove it.)
fn push_average(
    imu: &mut Imu,
    ts_sum: f64,
    raw_sum: &[u64; 6],
    n: usize,
    remainder: &mut [f64; 6],
) {
    imu.ts.push(ts_sum / n as f64);
    for (&sum, rest) in raw_sum.iter().zip(remainder.iter_mut()) {
        let mean = sum as f64 / n as f64 + *rest;
        let value = mean.round().clamp(0.0, u16::MAX as f64);
        *rest = mean - value;
        imu.raw.push(value as u16);
    }
}

/// The index of the exposure of the first video frame in `table`: the entry at the first-frame time of the
/// metadata (the end of its exposure). Without a first-frame time, the last `frame_count` entries.
fn first_exposure(
    table: &[Exposure],
    first_frame_us: Option<f64>,
    frame_count: usize,
) -> Option<usize> {
    let first = match first_frame_us {
        Some(first) => table
            .iter()
            .enumerate()
            .min_by(|a, b| {
                (a.1.timestamp_us - first)
                    .abs()
                    .total_cmp(&(b.1.timestamp_us - first).abs())
            })
            .map(|(i, _)| i)?,
        None => table.len().checked_sub(frame_count)?,
    };
    (frame_count > 0 && first + frame_count <= table.len()).then_some(first)
}

/// Tells if [`frame_times_us`] gets the frame times from the exposure table, not from the frame rate.
pub fn uses_exposure_table(trailer: &Trailer, frame_count: usize) -> bool {
    trailer
        .exposures
        .as_ref()
        .and_then(|t| first_exposure(t, trailer.first_frame_us, frame_count))
        .is_some()
}

/// Gives the time (µs, IMU clock) of each frame of lens 0 of a video with `frame_count` frames at `fps`.
/// As Insta360 (`GyroTimeConverter::ConvertToGyroTimeMs`): the middle of the exposure, `end - duration / 2`,
/// plus the gyro delay of the metadata. Frame 0 is the exposure that ends at the first-frame time. Without a long
/// enough exposure table, the first-frame time and the frame rate give the times.
pub fn frame_times_us(trailer: &Trailer, frame_count: usize, fps: f64) -> Vec<f64> {
    lens_frame_times(trailer, trailer.exposures.as_deref(), frame_count, fps)
}

/// Gives the frame times of each lens, see [`frame_times_us`]. Lens 1 uses its own exposure table
/// (`EXPOTIME_VIN1`) if the file has one, else the times of lens 0.
pub fn lens_frame_times_us(trailer: &Trailer, frame_count: usize, fps: f64) -> [Vec<f64>; 2] {
    let lens0 = frame_times_us(trailer, frame_count, fps);
    let lens1 = match trailer.exposures_lens1.as_deref() {
        Some(table) if first_exposure(table, trailer.first_frame_us, frame_count).is_some() => {
            lens_frame_times(trailer, Some(table), frame_count, fps)
        }
        _ => lens0.clone(),
    };
    [lens0, lens1]
}

fn lens_frame_times(
    trailer: &Trailer,
    table: Option<&[Exposure]>,
    frame_count: usize,
    fps: f64,
) -> Vec<f64> {
    let delay = trailer.gyro_delay_ms * 1e3;
    if let Some(table) = table {
        if let Some(first) = first_exposure(table, trailer.first_frame_us, frame_count) {
            return table[first..first + frame_count]
                .iter()
                .map(|e| e.timestamp_us - e.duration_s * 0.5e6 + delay)
                .collect();
        }
    }
    let start = trailer
        .first_frame_us
        .or_else(|| trailer.imu.as_ref().and_then(|imu| imu.ts.first().copied()))
        .unwrap_or(0.0);
    let step = if fps.is_finite() && fps > 0.0 {
        1e6 / fps
    } else {
        0.0
    };
    (0..frame_count)
        .map(|i| start + i as f64 * step + delay)
        .collect()
}

/// Reads an exposure table. A table with times that do not increase, or with a duration that is not valid, is
/// not used: then the frame times come from the frame rate.
fn read_exposures<R: Read + Seek>(
    reader: &mut R,
    location: Option<&(u64, u64)>,
) -> Result<Option<Vec<Exposure>>> {
    let Some(&(at, len)) = location else {
        return Ok(None);
    };
    if len == 0 || !len.is_multiple_of(EXPOSURE_BYTES) || len > MAX_EXPOSURE_BYTES {
        return Ok(None);
    }
    let bytes = read_at(reader, at, len as usize)?;
    let exposures: Vec<Exposure> = bytes
        .as_chunks::<16>()
        .0
        .iter()
        .map(|e| Exposure {
            timestamp_us: u64::from_le_bytes(e[..8].try_into().unwrap()) as f64,
            duration_s: f64::from_le_bytes(e[8..].try_into().unwrap()),
        })
        .collect();
    // The frame time is the middle of the exposure, thus the durations must be valid too.
    let valid = exposures
        .iter()
        .all(|e| e.duration_s.is_finite() && e.duration_s >= 0.0)
        && exposures
            .windows(2)
            .all(|w| w[1].timestamp_us > w[0].timestamp_us);
    Ok(valid.then_some(exposures))
}

fn read_imu_window<R: Read + Seek>(
    reader: &mut R,
    at: u64,
    len: u64,
    first_frame_us: Option<f64>,
) -> Result<Option<Imu>> {
    let total = (len / IMU_SAMPLE_BYTES as u64) as usize;
    if total == 0 {
        return Ok(None);
    }
    let mut imu = Imu::default();
    let mut limit = f64::INFINITY;
    let mut index = 0;
    'chunks: while index < total {
        let count = IMU_READ_CHUNK.min(total - index);
        let chunk = read_at(
            reader,
            at + (index * IMU_SAMPLE_BYTES) as u64,
            count * IMU_SAMPLE_BYTES,
        )?;
        for sample in chunk.as_chunks::<IMU_SAMPLE_BYTES>().0 {
            let ts = u64::from_le_bytes(sample[..8].try_into().unwrap()) as f64;
            if imu.ts.is_empty() {
                let first_frame = first_frame_us.unwrap_or(ts);
                limit = first_frame.clamp(ts, ts + MAX_FIRST_FRAME_DELAY_US) + IMU_WINDOW_US;
            }
            if ts > limit {
                break 'chunks;
            }
            imu.ts.push(ts);
            imu.raw.extend((0..6).map(|k| u16_at(sample, 8 + k * 2)));
        }
        index += count;
    }
    Ok((!imu.is_empty()).then_some(imu))
}

fn parse_metadata(metadata: &[u8]) -> Result<Trailer> {
    let mut trailer = Trailer {
        offset: [0.0; 16],
        offset_state: 0,
        local_time: None,
        first_frame_us: None,
        acc_range: 8.0,
        gyro_range: 2000.0,
        imu: None,
        exposures: None,
        exposures_lens1: None,
        rolling_shutter_ms: 0.0,
        gyro_delay_ms: 0.0,
        preview: None,
        imu_location: None,
    };
    let mut offset: Option<Vec<f64>> = None;
    for field in ProtobufFields::new(metadata) {
        match field? {
            (5, Value::Bytes(bytes)) if offset.is_none() => {
                offset = Some(
                    String::from_utf8_lossy(bytes)
                        .split('_')
                        .map(|v| v.parse().unwrap_or(f64::NAN))
                        .collect(),
                );
            }
            (7, Value::Varint(value)) => trailer.local_time = Some(value.to_string()),
            (24, Value::Varint(value)) => trailer.first_frame_us = Some(value as f64),
            (25, Value::Fixed64(value)) if value.is_finite() && value >= 0.0 => {
                trailer.rolling_shutter_ms = value
            }
            (28, Value::Fixed64(value)) if value.is_finite() => trailer.gyro_delay_ms = value,
            (68, Value::Varint(value)) => trailer.offset_state = value,
            (65, Value::Bytes(ranges)) => {
                for range in ProtobufFields::new(ranges) {
                    match range? {
                        (1, Value::Varint(value)) => trailer.acc_range = value as f64,
                        (2, Value::Varint(value)) => trailer.gyro_range = value as f64,
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    match offset {
        Some(values) if values.len() == 16 && values.iter().all(|v| v.is_finite()) => {
            trailer.offset.copy_from_slice(&values);
            Ok(trailer)
        }
        _ => unsupported("no ExtraMetadata calibration offset"),
    }
}

enum Value<'a> {
    Varint(u64),
    Fixed64(f64),
    Bytes(&'a [u8]),
}

/// A small reader for the protobuf wire format.
/// It reads varint, 64-bit (as f64) and length-delimited fields. It skips 32-bit fields.
struct ProtobufFields<'a> {
    buffer: &'a [u8],
    i: usize,
    done: bool,
}

impl<'a> ProtobufFields<'a> {
    fn new(buffer: &'a [u8]) -> Self {
        Self {
            buffer,
            i: 0,
            done: false,
        }
    }

    fn varint(&mut self) -> Result<u64> {
        let mut result = 0u64;
        let mut shift = 0;
        while self.i < self.buffer.len() {
            let byte = self.buffer[self.i];
            self.i += 1;
            if shift < 64 {
                result |= u64::from(byte & 0x7f) << shift;
            }
            if byte & 0x80 == 0 {
                return Ok(result);
            }
            shift += 7;
        }
        unsupported("truncated protobuf")
    }
}

impl<'a> Iterator for ProtobufFields<'a> {
    type Item = Result<(u64, Value<'a>)>;

    fn next(&mut self) -> Option<Self::Item> {
        while !self.done && self.i < self.buffer.len() {
            let tag = match self.varint() {
                Ok(tag) => tag,
                Err(error) => return Some(Err(error)),
            };
            let field = tag >> 3;
            match tag & 7 {
                0 => return Some(self.varint().map(|value| (field, Value::Varint(value)))),
                1 => {
                    let Some(bytes) = self.buffer.get(self.i..self.i + 8) else {
                        self.done = true;
                        continue;
                    };
                    self.i += 8;
                    let value = f64::from_le_bytes(bytes.try_into().unwrap());
                    return Some(Ok((field, Value::Fixed64(value))));
                }
                5 => self.i += 4,
                2 => {
                    let length = match self.varint() {
                        Ok(length) => length as usize,
                        Err(error) => return Some(Err(error)),
                    };
                    let end = self.i.saturating_add(length).min(self.buffer.len());
                    let bytes = &self.buffer[self.i..end];
                    self.i = end;
                    return Some(Ok((field, Value::Bytes(bytes))));
                }
                _ => self.done = true,
            }
        }
        None
    }
}
