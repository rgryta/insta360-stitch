use insta360_stitch::{write_spherical_mp4, write_spherical_mp4_file, Error};
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
    let mut table = (offsets.len() as u32).to_be_bytes().to_vec();
    for offset in offsets {
        table.extend(offset.to_be_bytes());
    }
    track_with(handler, entry, &full_box(b"stco", &table))
}

/// A track with a handler type, a sample entry and the chunk offset table `offsets`.
fn track_with(handler: &[u8; 4], entry: &[u8; 4], offsets: &[u8]) -> Vec<u8> {
    let hdlr = full_box(b"hdlr", &[&[0; 4][..], handler, &[0; 12], b"\0"].concat());
    let sample_entry = make_box(
        entry,
        &[&[0u8; 78][..], &make_box(b"avcC", &[1, 2, 3])].concat(),
    );
    let stsd = full_box(b"stsd", &[&1u32.to_be_bytes()[..], &sample_entry].concat());
    let stbl = make_box(b"stbl", &[&stsd[..], offsets].concat());
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

fn rewrite(file: Vec<u8>) -> Result<Vec<u8>, Error> {
    let mut out = Vec::new();
    write_spherical_mp4(&mut Cursor::new(file), &mut out).map(|()| out)
}

fn ftyp() -> Vec<u8> {
    make_box(b"ftyp", b"isom\0\0\x02\0isomiso2")
}

#[test]
fn moves_64_bit_chunk_offsets_and_reads_64_bit_box_sizes() {
    // mdat with a 64-bit size field, and a co64 table.
    let ftyp = ftyp();
    let mdat = [
        &1u32.to_be_bytes()[..],
        b"mdat",
        &(16u64 + 9).to_be_bytes(),
        b"VIDEO-ONE",
    ]
    .concat();
    let offset = (ftyp.len() + 16) as u64;
    let co64 = full_box(
        b"co64",
        &[&1u32.to_be_bytes()[..], &offset.to_be_bytes()].concat(),
    );
    let moov = make_box(b"moov", &track_with(b"vide", b"avc1", &co64));
    let out = rewrite([ftyp, mdat, moov].concat()).unwrap();
    let at = out.windows(4).position(|w| w == b"co64").unwrap();
    let moved = u64::from_be_bytes(out[at + 12..at + 20].try_into().unwrap()) as usize;
    assert_eq!(&out[moved..moved + 9], b"VIDEO-ONE");
}

#[test]
fn refuses_a_64_bit_box_size_that_wraps_around() {
    // 2^64 - 24 + 24 is 0 in u64: an addition in the bound check wraps, and the parser goes back to the start.
    let ftyp = ftyp();
    let huge = 0u64.wrapping_sub(ftyp.len() as u64);
    let file = [&ftyp[..], &1u32.to_be_bytes(), b"mdat", &huge.to_be_bytes()].concat();
    assert!(matches!(rewrite(file), Err(Error::Unsupported(_))));
}

#[test]
fn refuses_malformed_boxes_inside_moov() {
    let ftyp = ftyp();
    let mdat = make_box(b"mdat", b"VIDEO-ONE");
    let base = (ftyp.len() + 8) as u32;
    // A chunk offset table that ends after its version and flags, as the last box of the moov.
    let short_table = make_box(
        b"moov",
        &[
            track(b"vide", b"avc1", &[base]),
            track_with(b"soun", b"mp4a", &full_box(b"stco", &[])),
        ]
        .concat(),
    );
    // A video sample entry without the 78 bytes of visual fields.
    let short_entry = {
        let mut moov = make_box(b"moov", &track(b"vide", b"avc1", &[base]));
        let at = moov.windows(4).position(|w| w == b"avc1").unwrap() - 4;
        let size = u32::from_be_bytes(moov[at..at + 4].try_into().unwrap()) as usize;
        moov.drain(at + 8..at + size);
        moov[at..at + 4].copy_from_slice(&8u32.to_be_bytes());
        // Make the sizes of the ancestors agree again.
        let removed = (size - 8) as u32;
        for kind in [b"moov", b"trak", b"mdia", b"minf", b"stbl", b"stsd"] {
            let at = moov.windows(4).position(|w| w == kind).unwrap() - 4;
            let old = u32::from_be_bytes(moov[at..at + 4].try_into().unwrap());
            moov[at..at + 4].copy_from_slice(&(old - removed).to_be_bytes());
        }
        moov
    };
    // A fragmented file: the moov has an mvex box.
    let fragmented = make_box(
        b"moov",
        &[
            track(b"vide", b"avc1", &[base]),
            make_box(b"mvex", &full_box(b"trex", &[0; 20])),
        ]
        .concat(),
    );
    for moov in [short_table, short_entry, fragmented] {
        let result = rewrite([&ftyp[..], &mdat, &moov].concat());
        assert!(matches!(result, Err(Error::Unsupported(_))), "{result:?}");
    }
}

#[test]
fn adds_only_the_v2_boxes_to_a_file_with_v1_metadata() {
    let mut out = rewrite(mp4()).unwrap();
    // Remove the V2 boxes: change their type names. The file then has only the V1 metadata.
    for kind in [&b"st3d"[..], b"sv3d"] {
        let at = out.windows(4).position(|w| w == kind).unwrap();
        out[at..at + 4].copy_from_slice(b"free");
    }
    let again = rewrite(out).unwrap();
    assert_eq!(count(&again, b"GSpherical:Spherical>true"), 1);
    assert_eq!(count(&again, b"sv3d"), 1);
}

#[test]
fn the_file_form_changes_the_output_only_after_success() {
    let folder = std::env::temp_dir().join(format!("insta360-mp4-test-{}", std::process::id()));
    std::fs::create_dir_all(&folder).unwrap();
    let (input, output) = (folder.join("in.mp4"), folder.join("out.mp4"));
    std::fs::write(&input, mp4()).unwrap();
    write_spherical_mp4_file(&input, &output).unwrap();
    let good = std::fs::read(&output).unwrap();
    assert_eq!(good, rewrite(mp4()).unwrap());

    // A bad input does not change the output, and no temporary file stays.
    std::fs::write(&input, b"not an mp4 file at all").unwrap();
    let result = write_spherical_mp4_file(&input, &output);
    assert!(matches!(result, Err(Error::Unsupported(_))));
    assert_eq!(std::fs::read(&output).unwrap(), good);
    assert_eq!(std::fs::read_dir(&folder).unwrap().count(), 2);
    assert!(matches!(
        write_spherical_mp4_file(&output, &output),
        Err(Error::InvalidArgument(_))
    ));
    std::fs::remove_dir_all(&folder).unwrap();
}
