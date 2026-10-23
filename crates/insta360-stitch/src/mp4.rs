//! This module adds 360 video metadata to an MP4 file. Players then show the equirectangular video as a sphere.
//!
//! It adds the Google Spherical Video V2 boxes (`st3d` and `sv3d`) to the sample entry of the video track,
//! and the older V1 XML (`uuid` box) to the video track, for older players.
//! It also moves the `moov` box before the media data, as ffmpeg `-movflags +faststart` does.
//! Thus, the moved media data needs new chunk offsets in `stco` and `co64`.
//!
//! The module reads the `moov` box into memory. It copies the media data in sequence, thus a large video is not a problem.

use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::error::{invalid, unsupported, Result};

/// A `moov` box contains the sample tables. One hour of video needs some MB. A larger box is not real.
const MAX_MOOV_BYTES: u64 = 256 << 20;
/// A real MP4 file has some tens of boxes at one level. More boxes are an attack on the memory.
const MAX_BOXES: usize = 1 << 16;

const V1_UUID: [u8; 16] = [
    0xff, 0xcc, 0x82, 0x63, 0xf8, 0x55, 0x4a, 0x93, 0x88, 0x14, 0x58, 0x7a, 0x02, 0x52, 0x1f, 0xdd,
];
const V1_XML: &str = concat!(
    r#"<?xml version="1.0"?><rdf:SphericalVideo xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" "#,
    r#"xmlns:GSpherical="http://ns.google.com/videos/1.0/spherical/">"#,
    "<GSpherical:Spherical>true</GSpherical:Spherical><GSpherical:Stitched>true</GSpherical:Stitched>",
    "<GSpherical:StitchingSoftware>insta360-stitch</GSpherical:StitchingSoftware>",
    "<GSpherical:ProjectionType>equirectangular</GSpherical:ProjectionType></rdf:SphericalVideo>"
);

/// A box: the start, the full size and the header size, from the start of a buffer or a file.
#[derive(Debug, Clone, Copy)]
struct Item {
    start: u64,
    size: u64,
    header: u64,
    kind: [u8; 4],
}

impl Item {
    fn end(&self) -> u64 {
        self.start + self.size
    }
}

fn make_box(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + body.len());
    out.extend(((8 + body.len()) as u32).to_be_bytes());
    out.extend(kind);
    out.extend(body);
    out
}

fn full_box(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    make_box(kind, &[&[0, 0, 0, 0][..], body].concat())
}

/// The V2 boxes for a mono equirectangular video.
fn v2_boxes() -> Vec<u8> {
    let st3d = full_box(b"st3d", &[0]); // 0 = mono
    let svhd = full_box(b"svhd", b"insta360-stitch\0");
    let prhd = full_box(b"prhd", &[0; 12]); // yaw, pitch, roll = 0
    let equi = full_box(b"equi", &[0; 16]); // the full sphere: no bounds
    let proj = make_box(b"proj", &[prhd, equi].concat());
    let sv3d = make_box(b"sv3d", &[svhd, proj].concat());
    [st3d, sv3d].concat()
}

fn v1_box() -> Vec<u8> {
    make_box(b"uuid", &[&V1_UUID[..], V1_XML.as_bytes()].concat())
}

/// Reads the box header at `at`, in a buffer that ends at `end`.
fn item_at(buffer: &[u8], at: usize, end: usize) -> Result<Item> {
    if at + 8 > end {
        return unsupported("truncated MP4 box");
    }
    let size32 = u32::from_be_bytes(buffer[at..at + 4].try_into().unwrap());
    let kind: [u8; 4] = buffer[at + 4..at + 8].try_into().unwrap();
    let (size, header) = match size32 {
        0 => ((end - at) as u64, 8),
        1 => {
            if at + 16 > end {
                return unsupported("truncated MP4 box");
            }
            (
                u64::from_be_bytes(buffer[at + 8..at + 16].try_into().unwrap()),
                16,
            )
        }
        size => (u64::from(size), 8),
    };
    if size < header || size > (end - at) as u64 {
        return unsupported("invalid MP4 box size");
    }
    Ok(Item {
        start: at as u64,
        size,
        header,
        kind,
    })
}

/// Gives the child boxes of the box `parent`. `skip` is the number of bytes between the header and the first child.
fn children(buffer: &[u8], parent: &Item, skip: u64) -> Result<Vec<Item>> {
    let mut items = Vec::new();
    let mut at = (parent.start + parent.header + skip) as usize;
    let end = parent.end() as usize;
    while at + 8 <= end {
        if items.len() == MAX_BOXES {
            return unsupported("too many MP4 boxes");
        }
        let item = item_at(buffer, at, end)?;
        at = item.end() as usize;
        items.push(item);
    }
    Ok(items)
}

fn child(buffer: &[u8], parent: &Item, skip: u64, kind: &[u8; 4]) -> Result<Option<Item>> {
    Ok(children(buffer, parent, skip)?
        .into_iter()
        .find(|item| &item.kind == kind))
}

/// Adds `delta` bytes to the size field of a box.
fn grow(buffer: &mut [u8], item: &Item, delta: u64) -> Result<()> {
    let at = item.start as usize;
    if item.header == 16 {
        let size = item.size + delta;
        buffer[at + 8..at + 16].copy_from_slice(&size.to_be_bytes());
    } else {
        let Ok(size) = u32::try_from(item.size + delta) else {
            return unsupported("an MP4 box becomes too large");
        };
        buffer[at..at + 4].copy_from_slice(&size.to_be_bytes());
    }
    Ok(())
}

/// The box path from `moov` to the sample entry of the first video track.
struct VideoPath {
    trak: Item,
    boxes: Vec<Item>, // moov, trak, mdia, minf, stbl, stsd, sample entry
}

fn video_path(moov: &[u8]) -> Result<Option<VideoPath>> {
    let root = item_at(moov, 0, moov.len())?;
    for trak in children(moov, &root, 0)?
        .into_iter()
        .filter(|item| &item.kind == b"trak")
    {
        let Some(mdia) = child(moov, &trak, 0, b"mdia")? else {
            continue;
        };
        let Some(hdlr) = child(moov, &mdia, 0, b"hdlr")? else {
            continue;
        };
        // hdlr: version and flags (4), pre_defined (4), handler type (4)
        let at = (hdlr.start + hdlr.header + 8) as usize;
        if hdlr.size < hdlr.header + 12 || &moov[at..at + 4] != b"vide" {
            continue;
        }
        let path = (|| -> Result<Option<Vec<Item>>> {
            let Some(minf) = child(moov, &mdia, 0, b"minf")? else {
                return Ok(None);
            };
            let Some(stbl) = child(moov, &minf, 0, b"stbl")? else {
                return Ok(None);
            };
            let Some(stsd) = child(moov, &stbl, 0, b"stsd")? else {
                return Ok(None);
            };
            // stsd: version and flags (4), entry count (4), then the sample entries
            let Some(entry) = children(moov, &stsd, 8)?.into_iter().next() else {
                return Ok(None);
            };
            Ok(Some(vec![root, trak, mdia, minf, stbl, stsd, entry]))
        })()?;
        if let Some(boxes) = path {
            return Ok(Some(VideoPath { trak, boxes }));
        }
    }
    Ok(None)
}

/// Inserts `data` at `at` and adds its length to the sizes of `ancestors`.
fn insert(moov: &mut Vec<u8>, ancestors: &[Item], at: usize, data: &[u8]) -> Result<()> {
    for item in ancestors {
        grow(moov, item, data.len() as u64)?;
    }
    moov.splice(at..at, data.iter().copied());
    Ok(())
}

/// Adds the V2 boxes to the video sample entry and the V1 box to the video track.
/// It gives `false` if the moov already has 360 metadata.
fn add_metadata(moov: &mut Vec<u8>) -> Result<bool> {
    let Some(path) = video_path(moov)? else {
        return unsupported("the MP4 file has no video track");
    };
    let entry = *path.boxes.last().unwrap();
    // A visual sample entry has 78 bytes of fields before its child boxes.
    if entry.size < entry.header + 78 {
        return unsupported("the video sample entry is too short");
    }
    if children(moov, &entry, 78)?
        .iter()
        .any(|item| &item.kind == b"sv3d")
    {
        return Ok(false);
    }
    insert(moov, &path.boxes, entry.end() as usize, &v2_boxes())?;
    // The video track is now larger: read the path again, then add the V1 box at the end of the track.
    let path = video_path(moov)?.unwrap();
    let has_v1 = children(moov, &path.trak, 0)?.iter().any(|item| {
        let at = (item.start + item.header) as usize;
        &item.kind == b"uuid" && item.size >= item.header + 16 && moov[at..at + 16] == V1_UUID
    });
    if !has_v1 {
        insert(moov, &path.boxes[..2], path.trak.end() as usize, &v1_box())?;
    }
    Ok(true)
}

/// Changes each chunk offset in `stco` and `co64` with `map`.
fn move_chunk_offsets(moov: &mut [u8], map: impl Fn(u64) -> Result<u64>) -> Result<()> {
    let root = item_at(moov, 0, moov.len())?;
    let mut tables = Vec::new();
    for trak in children(moov, &root, 0)?
        .into_iter()
        .filter(|item| &item.kind == b"trak")
    {
        let stbl = (|| -> Result<Option<Item>> {
            let Some(mdia) = child(moov, &trak, 0, b"mdia")? else {
                return Ok(None);
            };
            let Some(minf) = child(moov, &mdia, 0, b"minf")? else {
                return Ok(None);
            };
            child(moov, &minf, 0, b"stbl")
        })()?;
        if let Some(stbl) = stbl {
            tables.extend(
                children(moov, &stbl, 0)?
                    .into_iter()
                    .filter(|item| matches!(&item.kind, b"stco" | b"co64")),
            );
        }
    }
    for table in tables {
        let wide = &table.kind == b"co64";
        // version and flags (4), entry count (4), then the offsets
        if table.size < table.header + 8 {
            return unsupported("truncated MP4 chunk offset table");
        }
        let at = (table.start + table.header + 4) as usize;
        let count = u32::from_be_bytes(moov[at..at + 4].try_into().unwrap()) as usize;
        let width = if wide { 8 } else { 4 };
        if (count * width) as u64 > table.size - table.header - 8 {
            return unsupported("truncated MP4 chunk offset table");
        }
        for i in 0..count {
            let p = at + 4 + i * width;
            if wide {
                let offset = u64::from_be_bytes(moov[p..p + 8].try_into().unwrap());
                moov[p..p + 8].copy_from_slice(&map(offset)?.to_be_bytes());
            } else {
                let offset = u64::from(u32::from_be_bytes(moov[p..p + 4].try_into().unwrap()));
                let Ok(moved) = u32::try_from(map(offset)?) else {
                    return unsupported("a chunk offset becomes larger than 4 GB");
                };
                moov[p..p + 4].copy_from_slice(&moved.to_be_bytes());
            }
        }
    }
    Ok(())
}

fn top_level<R: Read + Seek>(input: &mut R) -> Result<Vec<Item>> {
    let size = input.seek(SeekFrom::End(0))?;
    let mut items = Vec::new();
    let mut at = 0u64;
    while at + 8 <= size {
        input.seek(SeekFrom::Start(at))?;
        let mut head = [0u8; 16];
        input.read_exact(&mut head[..8])?;
        let size32 = u32::from_be_bytes(head[..4].try_into().unwrap());
        let kind: [u8; 4] = head[4..8].try_into().unwrap();
        let (box_size, header) = match size32 {
            0 => (size - at, 8),
            1 => {
                input.read_exact(&mut head[8..16])?;
                (u64::from_be_bytes(head[8..16].try_into().unwrap()), 16)
            }
            box_size => (u64::from(box_size), 8),
        };
        if box_size < header || box_size > size - at {
            return unsupported("invalid MP4 box size");
        }
        if items.len() == MAX_BOXES {
            return unsupported("too many MP4 boxes");
        }
        items.push(Item {
            start: at,
            size: box_size,
            header,
            kind,
        });
        at += box_size;
    }
    Ok(items)
}

/// Copies the MP4 file `input` to `output` and adds the 360 video metadata.
/// The output has `ftyp`, then `moov`, then the other boxes, thus a player can start before the full download.
/// If the input already has the metadata, the function only puts `moov` first.
/// It refuses a fragmented MP4 file, because it does not move the data offsets in the fragments.
pub fn write_spherical_mp4<R: Read + Seek, W: Write>(input: &mut R, output: &mut W) -> Result<()> {
    let items = top_level(input)?;
    let (Some(ftyp), true) = (
        items.first().copied(),
        items.first().is_some_and(|item| &item.kind == b"ftyp"),
    ) else {
        return unsupported("the file is not an MP4 file (no ftyp box first)");
    };
    let mut moovs = items.iter().filter(|item| &item.kind == b"moov");
    let (Some(&moov_item), None) = (moovs.next(), moovs.next()) else {
        return unsupported("the MP4 file must have exactly one moov box");
    };
    if moov_item.size > MAX_MOOV_BYTES {
        return unsupported("the MP4 moov box is too large");
    }
    input.seek(SeekFrom::Start(moov_item.start))?;
    let mut moov = vec![0u8; moov_item.size as usize];
    input.read_exact(&mut moov)?;
    // A fragmented file has more data offsets in its fragments. This module does not move them.
    let fragmented = children(&moov, &item_at(&moov, 0, moov.len())?, 0)?
        .iter()
        .any(|item| &item.kind == b"mvex");
    if fragmented || items.iter().any(|item| &item.kind == b"moof") {
        return unsupported("fragmented MP4 files are not supported");
    }
    add_metadata(&mut moov)?;

    // The new order: ftyp, moov, then the other boxes in their old order.
    let rest: Vec<Item> = items
        .iter()
        .skip(1)
        .filter(|item| &item.kind != b"moov")
        .copied()
        .collect();
    let mut moves = Vec::new(); // (old start, old end, new start)
    let mut at = ftyp.size + moov.len() as u64;
    for item in &rest {
        moves.push((item.start, item.end(), at));
        at += item.size;
    }
    move_chunk_offsets(&mut moov, |offset| {
        match moves
            .iter()
            .find(|&&(start, end, _)| start <= offset && offset < end)
        {
            Some(&(start, _, new_start)) => Ok(new_start + (offset - start)),
            None => unsupported("a chunk offset points outside the media data"),
        }
    })?;

    copy_range(input, output, ftyp.start, ftyp.size)?;
    output.write_all(&moov)?;
    for item in &rest {
        copy_range(input, output, item.start, item.size)?;
    }
    output.flush()?;
    Ok(())
}

/// Refuses an output path that is the input file. Creating the output would delete the input before the read.
/// A hard link to the input is not found.
pub fn check_different_files(input: &Path, output: &Path) -> Result<()> {
    let same = match (input.canonicalize(), output.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => input == output,
    };
    if same {
        return invalid("the output must be a different file than the input");
    }
    Ok(())
}

/// Copies the MP4 file at `input` to the file `output` and adds the 360 video metadata. See [`write_spherical_mp4`].
/// It writes a temporary file next to `output`, and then renames it. Thus, after an error, `output` did not change.
pub fn write_spherical_mp4_file(input: &Path, output: &Path) -> Result<()> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    check_different_files(input, output)?;
    let mut source = BufReader::new(File::open(input)?);
    // Each call has its own temporary file. Thus, two calls with the same output do not mix their data.
    let mut part = output.as_os_str().to_owned();
    part.push(format!(
        ".{}-{}.part",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        let mut target = BufWriter::new(File::create(&part)?);
        write_spherical_mp4(&mut source, &mut target)?;
        target
            .into_inner()
            .map_err(|e| e.into_error())?
            .sync_all()?;
        Ok(std::fs::rename(&part, output)?)
    })();
    if result.is_err() {
        std::fs::remove_file(&part).ok();
    }
    result
}

fn copy_range<R: Read + Seek, W: Write>(
    input: &mut R,
    output: &mut W,
    start: u64,
    size: u64,
) -> Result<()> {
    input.seek(SeekFrom::Start(start))?;
    let copied = io::copy(&mut input.by_ref().take(size), output)?;
    if copied != size {
        return Err(
            io::Error::new(io::ErrorKind::UnexpectedEof, "short read of the MP4 file").into(),
        );
    }
    Ok(())
}
