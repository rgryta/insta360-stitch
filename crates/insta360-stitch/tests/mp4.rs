use insta360_stitch::{write_spherical_mp4, Error};
use std::io::Cursor;

fn make_box(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut out = ((8 + body.len()) as u32).to_be_bytes().to_vec();
    out.extend(kind);
    out.extend(body);
    out
}

fn full_box(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    make_box(kind, &[&[0, 0, 0, 0][..], body].concat())
}

/// A track with a handler type, a sample entry and a `stco` chunk offset table.
fn track(handler: &[u8; 4], entry: &[u8; 4], offsets: &[u32]) -> Vec<u8> {
    let hdlr = full_box(b"hdlr", &[&[0; 4][..], handler, &[0; 12], b"\0"].concat());
    let sample_entry = make_box(
        entry,
        &[&[0u8; 78][..], &make_box(b"avcC", &[1, 2, 3])].concat(),
    );
    let stsd = full_box(b"stsd", &[&1u32.to_be_bytes()[..], &sample_entry].concat());
    let mut table = (offsets.len() as u32).to_be_bytes().to_vec();
    for offset in offsets {
        table.extend(offset.to_be_bytes());
    }
    let stbl = make_box(b"stbl", &[stsd, full_box(b"stco", &table)].concat());
    let mdia = make_box(b"mdia", &[hdlr, make_box(b"minf", &stbl)].concat());
    make_box(b"trak", &[full_box(b"tkhd", &[0; 8]), mdia].concat())
}

/// An MP4 file as ffmpeg writes it without faststart: ftyp, mdat, then moov.
fn mp4() -> Vec<u8> {
    let ftyp = make_box(b"ftyp", b"isom\0\0\x02\0isomiso2");
    let mdat = make_box(b"mdat", b"VIDEO-ONEAUDIO-ONEVIDEO-TWO");
    let base = (ftyp.len() + 8) as u32;
    let moov = make_box(
        b"moov",
        &[
            full_box(b"mvhd", &[0; 8]),
            track(b"vide", b"avc1", &[base, base + 18]),
            track(b"soun", b"mp4a", &[base + 9]),
        ]
        .concat(),
    );
    [ftyp, mdat, moov].concat()
}

/// The top-level box types, and the chunk offsets of all `stco` tables, in file order.
fn layout(file: &[u8]) -> (Vec<String>, Vec<u32>) {
    let mut kinds = Vec::new();
    let mut at = 0;
    while at + 8 <= file.len() {
        let size = u32::from_be_bytes(file[at..at + 4].try_into().unwrap()) as usize;
        kinds.push(String::from_utf8_lossy(&file[at + 4..at + 8]).into_owned());
        at += size;
    }
    let mut offsets = Vec::new();
    for at in (0..file.len() - 4).filter(|&i| &file[i..i + 4] == b"stco") {
        let count = u32::from_be_bytes(file[at + 8..at + 12].try_into().unwrap()) as usize;
        for i in 0..count {
            let p = at + 12 + i * 4;
            offsets.push(u32::from_be_bytes(file[p..p + 4].try_into().unwrap()));
        }
    }
    (kinds, offsets)
}

fn count(file: &[u8], needle: &[u8]) -> usize {
    file.windows(needle.len()).filter(|w| *w == needle).count()
}

#[test]
fn adds_360_metadata_and_moves_moov_before_the_media_data() {
    let mut out = Vec::new();
    write_spherical_mp4(&mut Cursor::new(mp4()), &mut out).unwrap();
    let (kinds, offsets) = layout(&out);
    assert_eq!(kinds, ["ftyp", "moov", "mdat"]);
    // Each chunk offset, also in the audio track, still points to its data.
    let chunks: Vec<&[u8]> = offsets
        .iter()
        .map(|&o| &out[o as usize..o as usize + 9])
        .collect();
    assert_eq!(chunks, [&b"VIDEO-ONE"[..], b"VIDEO-TWO", b"AUDIO-ONE"]);
    assert_eq!(
        (
            count(&out, b"st3d"),
            count(&out, b"sv3d"),
            count(&out, b"equi\0\0\0\0")
        ),
        (1, 1, 1)
    );
    assert_eq!(count(&out, b"GSpherical:Spherical>true"), 1);
    // The V2 boxes are in the video sample entry, not in the audio track.
    let avc1 = out.windows(4).position(|w| w == b"avc1").unwrap();
    let mp4a = out.windows(4).position(|w| w == b"mp4a").unwrap();
    let sv3d = out.windows(4).position(|w| w == b"sv3d").unwrap();
    assert!(avc1 < sv3d && sv3d < mp4a);

    // A second run does not add the metadata again.
    let mut again = Vec::new();
    write_spherical_mp4(&mut Cursor::new(out.clone()), &mut again).unwrap();
    assert_eq!(again, out);
}

#[test]
fn refuses_a_file_that_is_not_an_mp4() {
    let mut out = Vec::new();
    let result = write_spherical_mp4(
        &mut Cursor::new(b"not an mp4 file at all".to_vec()),
        &mut out,
    );
    assert!(matches!(result, Err(Error::Unsupported(_))));
}
