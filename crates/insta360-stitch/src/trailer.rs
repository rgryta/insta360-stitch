//! The proprietary trailer Insta360 appends to its .insp (JPEG) and .insv (MP4) files.
//!
//! Layout: records whose `[u16 id][u32 length]` header follows the payload, walked backwards
//! from a 78-byte tail (`u32` total length at offset 38, a 32-byte magic at offset 46). Videos
//! replace the walk with a directory record (id 0) of `[u16 id][u32 size][u32 offset]` entries.
//!
//! Reading is record by record: only the headers, the metadata, the IMU up to the first frame
//! plus a window, and (on request) the preview are read, never the whole trailer, which grows
//! with recording length.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};

use crate::error::{unsupported, Error, Result};

/// Bytes at the end of every Insta360 file that locate the trailer.
pub const TAIL_LENGTH: usize = 78;
const MAGIC: &[u8; 32] = b"8db42d694ccc418790edff439fe026bf";

const RECORD_DIRECTORY: u16 = 0x0;
const RECORD_EXTRA_METADATA: u16 = 0x101;
const RECORD_PREVIEW: u16 = 0x200;
const RECORD_IMU: u16 = 0x300;
const IMU_SAMPLE_BYTES: usize = 20;
const PREVIEW_HEADER_BYTES: u64 = 40;
/// IMU decoded past the first frame: the 0.02 Hz zero-phase low-pass gives later samples no
/// weight, and a long recording's whole IMU stream is ~1 kHz for its entire length.
const IMU_WINDOW_US: f64 = 120e6;
const IMU_READ_CHUNK: usize = 4096;

/// The raw motion sensor stream: timestamps (µs) and 6 raw `u16` values per sample
/// (accelerometer x/y/z, then gyroscope x/y/z, 32768 = zero).
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

/// The camera's own small stitched equirectangular frame, NV12 (BT.601, limited range).
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

/// Everything the stitcher needs from a trailer.
#[derive(Debug, Clone)]
pub struct Trailer {
    /// Calibration: count, [radius cx cy rx ry rz] x2, calibration width, height, lens word.
    pub offset: [f64; 16],
    /// Lens guard / accessory state (0 = none, 4 = standard guard, 5 = premium guard, ...).
    pub offset_state: u64,
    /// The camera's wall clock when the file was created, as `YYYYMMDDhhmmss` digits.
    pub local_time: Option<String>,
    /// When the first frame was exposed, on the IMU clock (µs).
    pub first_frame_us: Option<f64>,
    /// Accelerometer full-scale range (g).
    pub acc_range: f64,
    /// Gyroscope full-scale range (degrees per second).
    pub gyro_range: f64,
    pub imu: Option<Imu>,
    pub(crate) preview: Option<PreviewLocation>,
}

impl Trailer {
    /// Width and height of the camera's embedded preview, if the trailer has one.
    pub fn preview_size(&self) -> Option<(u32, u32)> {
        self.preview.map(|p| (p.width, p.height))
    }
}

/// The trailer's total length, from a file's last 78 bytes; validated before anything that size is read.
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
    // a short read is an I/O error (worth a retry), never "no records"
    reader.read_exact(&mut buffer).map_err(Error::Io)?;
    Ok(buffer)
}

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

/// Reads a trailer from an Insta360 file (or any seekable byte source holding one).
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
    let metadata = read_at(reader, metadata_at, metadata_len as usize)?;
    let mut trailer = parse_metadata(&metadata)?;

    if let Some(&(imu_at, imu_len)) = records.get(&RECORD_IMU) {
        trailer.imu = read_imu(reader, imu_at, imu_len, trailer.first_frame_us)?;
    }

    if let Some(&(preview_at, preview_len)) = records.get(&RECORD_PREVIEW) {
        if preview_len >= PREVIEW_HEADER_BYTES {
            // [u32 1][u32 record length][u32 1][u32 0][u32 width][u32 height], padded to 40 bytes, then NV12
            let header = read_at(reader, preview_at, PREVIEW_HEADER_BYTES as usize)?;
            let (width, height) = (u32_at(&header, 16), u32_at(&header, 20));
            let pixels = u64::from(width) * u64::from(height);
            if u32_at(&header, 0) == 1
                && pixels > 0
                && preview_len == PREVIEW_HEADER_BYTES + pixels * 3 / 2
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

/// Parses a trailer already in memory (the bytes from the start of the trailer to the end of the file).
pub fn parse_trailer(bytes: &[u8]) -> Result<Trailer> {
    read_trailer(&mut std::io::Cursor::new(bytes))
}

/// Reads the camera's embedded preview; `reader` must be the source `trailer` was read from.
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

/// Record id -> (absolute offset, length), from the headers only.
fn index_records<R: Read + Seek>(
    reader: &mut R,
    start: u64,
    length: u64,
) -> Result<HashMap<u16, (u64, u64)>> {
    let mut records = HashMap::new();
    let mut header = length - TAIL_LENGTH as u64;
    while header >= 6 {
        let bytes = read_at(reader, start + header, 6)?;
        let (id, record_len) = (u16_at(&bytes, 0), u64::from(u32_at(&bytes, 2)));
        if record_len > header {
            break;
        }
        if id == RECORD_DIRECTORY {
            let table = read_at(reader, start + header - record_len, record_len as usize)?;
            for entry in table.chunks_exact(10) {
                let size = u64::from(u32_at(entry, 2));
                let offset = u64::from(u32_at(entry, 6));
                if size > 0 && offset + size + 6 <= length {
                    // the table abbreviates ids (0x3 for 0x300); the record's own footer has the full one
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

fn read_imu<R: Read + Seek>(
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
        for sample in chunk.chunks_exact(IMU_SAMPLE_BYTES) {
            let ts = u64::from_le_bytes(sample[..8].try_into().unwrap()) as f64;
            if imu.ts.is_empty() {
                limit = first_frame_us.unwrap_or(ts).max(ts) + IMU_WINDOW_US;
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
        preview: None,
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
    Bytes(&'a [u8]),
}

/// A minimal protobuf wire-format reader: varints and length-delimited fields; fixed-width ones are skipped.
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
                1 => self.i += 8,
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
