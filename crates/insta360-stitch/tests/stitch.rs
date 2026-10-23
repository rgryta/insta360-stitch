mod common;

use common::*;
use insta360_stitch::math::{qrotate, Vec3, IDENTITY};
use insta360_stitch::*;
use std::io::Cursor;

fn degrees_between(a: &Vec3, b: &Vec3) -> f64 {
    let d = (a[0] * b[0] + a[1] * b[1] + a[2] * b[2]).clamp(-1.0, 1.0);
    d.acos().to_degrees()
}

/// A camera that turns at a constant rate. In the body frame, the gravity turns in the opposite direction.
fn turning_camera(n: usize) -> (Vec<f64>, Vec<Vec3>, Vec<Vec3>) {
    let w: Vec3 = [0.3, -0.5, 0.8];
    let angle = (w[0] * w[0] + w[1] * w[1] + w[2] * w[2]).sqrt();
    let g0: Vec3 = [0.2, -0.3, -0.93];
    let n0 = (g0[0] * g0[0] + g0[1] * g0[1] + g0[2] * g0[2]).sqrt();
    let ts: Vec<f64> = (0..n).map(|i| i as f64 * 0.001).collect();
    let acc = ts
        .iter()
        .map(|t| {
            let s = (angle * t / 2.0).sin() / angle;
            qrotate(
                &[-w[0] * s, -w[1] * s, -w[2] * s, (angle * t / 2.0).cos()],
                &[g0[0] / n0, g0[1] / n0, g0[2] / n0],
            )
        })
        .collect();
    (ts, acc, vec![w; n])
}

#[test]
fn filtfilt_keeps_a_constant_and_matches_scipy() {
    let filter = butter2_lowpass(0.02, 1000.0);
    let constant = filtfilt2(&filter, &[0.3; 5000]);
    assert!(constant.iter().all(|v| (v - 0.3).abs() < 1e-6));
    // The reference values come from scipy.signal.filtfilt(..., padtype='odd', padlen=6) on linspace(-1, 1, 5000).
    let ramp: Vec<f64> = (0..5000).map(|i| -1.0 + 2.0 * i as f64 / 4999.0).collect();
    let out = filtfilt2(&filter, &ramp);
    for (i, expected) in [(0, -0.906286), (2500, -0.898521), (4999, -0.896801)] {
        assert!((out[i] - expected).abs() < 5e-6, "{i}: {}", out[i]);
    }
}

#[test]
fn levels_a_turning_camera_to_its_measured_gravity() {
    let (ts, acc, gyr) = turning_camera(12_000);
    let GravityQuats { quats, .. } = box_gravity_quats(&ts, &acc, &gyr).unwrap();
    for k in [0, 4000, 11_999] {
        let up = qrotate(&quats[k], &[0.0, 0.0, 1.0]);
        assert!(degrees_between(&up, &[-acc[k][0], -acc[k][1], -acc[k][2]]) < 0.05);
    }
}

#[test]
fn falls_back_when_the_imu_buffer_is_shorter_than_the_gravity_box() {
    let (ts, acc, gyr) = turning_camera(5000);
    assert!(box_gravity_quats(&ts, &acc, &gyr).is_none());
}

#[test]
fn parses_calibration_ranges_clock_and_imu() {
    let ranges = [field_varint(1, 32), field_varint(2, 2000)].concat();
    let bytes = trailer(&[
        record(0x300, &imu_samples(2, 1000, [0, 0, -1024], 1_000_000)),
        metadata(&[
            field_bytes(5, OFFSET.as_bytes()),
            field_varint(7, 20_260_821_105_307),
            field_varint(24, 1_000_000),
            field_bytes(65, &ranges),
        ]),
    ]);
    let t = parse_trailer(&bytes).unwrap();
    assert_eq!(t.offset[15], 1137.0);
    assert_eq!(t.local_time.as_deref(), Some("20260821105307"));
    assert_eq!(
        (t.first_frame_us, t.acc_range, t.gyro_range),
        (Some(1_000_000.0), 32.0, 2000.0)
    );
    assert_eq!(t.imu.as_ref().unwrap().ts, vec![1_000_000.0, 1_001_000.0]);

    // Two samples are not sufficient for the gravity window. The function uses the first sample and gives a proper rotation.
    let r = orientation(&t).unwrap();
    let det = r[0][0] * (r[1][1] * r[2][2] - r[1][2] * r[2][1])
        - r[0][1] * (r[1][0] * r[2][2] - r[1][2] * r[2][0])
        + r[0][2] * (r[1][0] * r[2][1] - r[1][1] * r[2][0]);
    assert!((det - 1.0).abs() < 1e-9);
    let mut without_imu = t.clone();
    without_imu.imu = None;
    assert!(matches!(
        orientation(&without_imu),
        Err(Error::Unsupported(_))
    ));
}

#[test]
fn finds_video_records_through_the_directory_table() {
    let meta = field_bytes(5, OFFSET.as_bytes());
    let imu = imu_samples(1, 1000, [0, 0, -1024], 0);
    let body = [record(0x101, &meta), record(0x300, &imu)].concat();
    let mut directory = Vec::new();
    for (id, size, offset) in [
        (0x1u16, meta.len() as u32, 0u32),
        (0x3, imu.len() as u32, meta.len() as u32 + 6),
    ] {
        directory.extend(id.to_le_bytes());
        directory.extend(size.to_le_bytes());
        directory.extend(offset.to_le_bytes());
    }
    let t = parse_trailer(&trailer(&[body, record(0x0, &directory)])).unwrap();
    assert_eq!(t.offset[15], 1137.0);
    assert_eq!(t.imu.unwrap().len(), 1);
}

#[test]
fn reads_the_nv12_preview_only_on_request() {
    let mut header = Vec::new();
    for value in [1u32, 46, 1, 0, 2, 2] {
        header.extend(value.to_le_bytes());
    }
    header.resize(40, 0);
    // A 2x2 image: black, white, mid gray, mid gray, with one neutral chroma pair.
    let preview = [header, vec![16, 235, 126, 126, 128, 128]].concat();
    let bytes = trailer(&[
        record(0x200, &preview),
        metadata(&[field_bytes(5, OFFSET.as_bytes())]),
    ]);
    let t = parse_trailer(&bytes).unwrap();
    assert_eq!(t.preview_size(), Some((2, 2)));
    let p = read_preview(&mut Cursor::new(&bytes), &t).unwrap();
    assert_eq!(
        preview_rgb(&p),
        vec![0, 0, 0, 255, 255, 255, 128, 128, 128, 128, 128, 128]
    );
}

#[test]
fn ignores_a_preview_with_an_odd_size() {
    // A 2x3 NV12 image has 9 bytes, but its last chroma row needs one more byte.
    let mut header = Vec::new();
    for value in [1u32, 49, 1, 0, 2, 3] {
        header.extend(value.to_le_bytes());
    }
    header.resize(40, 0);
    let preview = [header, vec![128; 9]].concat();
    let bytes = trailer(&[
        record(0x200, &preview),
        metadata(&[field_bytes(5, OFFSET.as_bytes())]),
    ]);
    assert_eq!(parse_trailer(&bytes).unwrap().preview_size(), None);
}

#[test]
fn trusts_the_trailer_length_only_behind_the_magic() {
    let mut tail = vec![0u8; 78];
    tail[38..42].copy_from_slice(&150u32.to_le_bytes());
    assert!(matches!(
        trailer_length(&tail, 200),
        Err(Error::Unsupported(_))
    ));
    tail[46..].copy_from_slice(b"8db42d694ccc418790edff439fe026bf");
    assert_eq!(trailer_length(&tail, 200).unwrap(), 150);
    assert!(matches!(
        trailer_length(&tail, 100),
        Err(Error::Unsupported(_))
    ));
}

#[test]
fn a_file_without_a_trailer_is_unsupported_and_a_truncated_one_is_an_io_error() {
    assert!(matches!(
        parse_trailer(b"no trailer"),
        Err(Error::Unsupported(_))
    ));
    // The tail shows a trailer, but the source gives fewer bytes than the records need.
    let bytes = trailer(&[metadata(&[field_bytes(5, OFFSET.as_bytes())])]);
    struct Short(Cursor<Vec<u8>>);
    impl std::io::Read for Short {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = buf.len().min(8); // Give a small number of bytes. Stop on large reads.
            if buf.len() > 100 {
                return Ok(0);
            }
            self.0.read(&mut buf[..n])
        }
    }
    impl std::io::Seek for Short {
        fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
            self.0.seek(pos)
        }
    }
    let mut padded = vec![0u8; 200];
    padded.extend(&bytes);
    assert!(matches!(
        read_trailer(&mut Short(Cursor::new(padded))),
        Err(Error::Io(_))
    ));
}

#[test]
fn refuses_a_zero_gravity_sample_instead_of_stitching_black() {
    let bytes = trailer(&[
        record(0x300, &imu_samples(1, 1000, [0, 0, 0], 1_000_000)),
        metadata(&[field_bytes(5, OFFSET.as_bytes())]),
    ]);
    assert!(matches!(
        orientation(&parse_trailer(&bytes).unwrap()),
        Err(Error::Unsupported(_))
    ));
}

fn lens_fov(offset_state: u64) -> Result<f64> {
    let bytes = trailer(&[metadata(&[
        field_bytes(5, OFFSET.as_bytes()),
        field_varint(68, offset_state),
    ])]);
    LensModel::new(&parse_trailer(&bytes).unwrap(), 1000, IDENTITY).map(|m| m.half_fov)
}

#[test]
fn picks_the_lens_fov_from_the_lens_guard_and_refuses_unmeasured_accessories() {
    assert_eq!(lens_fov(0).unwrap(), 100.0);
    assert_eq!(lens_fov(5).unwrap(), 97.5);
    assert!(
        matches!(lens_fov(4), Err(Error::Unsupported(reason)) if reason.contains("offset state 4"))
    );
}

#[test]
fn refuses_a_calibration_that_would_stitch_black() {
    let bytes = trailer(&[metadata(&[field_bytes(
        5,
        OFFSET.replace("_10752_", "_0_").as_bytes(),
    )])]);
    assert!(matches!(
        LensModel::new(&parse_trailer(&bytes).unwrap(), 1000, IDENTITY),
        Err(Error::Unsupported(_))
    ));
}

#[test]
fn decodes_the_imu_only_up_to_two_minutes_past_the_first_frame() {
    let ranges = field_varint(1, 32);
    let meta = metadata(&[
        field_bytes(5, OFFSET.as_bytes()),
        field_varint(24, 2_000_000),
        field_bytes(65, &ranges),
    ]);
    // The samples are at 100 Hz from 1 s to 123 s. Then the camera turns on its side.
    // These last samples are after the window. Thus, they must not change the result.
    let kept = imu_samples(12_200, 10_000, [0, 0, -1024], 1_000_000);
    let past = imu_samples(800, 10_000, [1024, 0, 0], 1_000_000 + 12_200 * 10_000);
    let full = parse_trailer(&trailer(&[
        record(0x300, &[kept.clone(), past].concat()),
        meta.clone(),
    ]))
    .unwrap();
    let cut = parse_trailer(&trailer(&[record(0x300, &kept), meta])).unwrap();
    assert!(*full.imu.as_ref().unwrap().ts.last().unwrap() <= 2_000_000.0 + 120e6);
    assert_eq!(orientation(&full).unwrap(), orientation(&cut).unwrap());
}

#[test]
fn gives_each_pixel_to_the_lens_that_can_see_it() {
    assert_eq!(blend_weight(true, true, 0.25), 0.25);
    assert_eq!(blend_weight(true, false, 0.0), 1.0);
    assert_eq!(blend_weight(false, true, 1.0), 0.0);
    assert_eq!(blend_weight(false, false, 0.5), 0.0);
}

#[test]
fn streamed_rows_match_the_whole_image_stitch() {
    let bytes = trailer(&[
        record(0x300, &imu_samples(2, 1000, [0, 0, -1024], 1_000_000)),
        metadata(&[field_bytes(5, OFFSET.as_bytes())]),
    ]);
    let t = parse_trailer(&bytes).unwrap();
    let (width, height) = (64, 32);
    let pixels: Vec<u8> = (0..width * height * 3)
        .map(|i| (i * 7 % 251) as u8)
        .collect();
    let source = Image {
        width,
        height,
        channels: 3,
        data: &pixels,
    };
    let model = LensModel::new(&t, height, orientation(&t).unwrap()).unwrap();
    let whole = stitch_photo(&source, &model).unwrap();
    let mut streamed = Vec::new();
    stitch_photo_rows(&source, &model, 5, |y, row| {
        assert_eq!(streamed.len(), y * row.len());
        streamed.extend_from_slice(row);
        Ok(())
    })
    .unwrap();
    assert_eq!(whole, streamed);
    // A band in the middle gives the same rows as the full stitch.
    let row_len = width * 3;
    let mut band = vec![0u8; 7 * row_len];
    stitch_photo_band(&source, &model, 10, &mut band).unwrap();
    assert_eq!(band, whole[10 * row_len..17 * row_len]);
    // A band that goes past the last row is an error.
    assert!(matches!(
        stitch_photo_band(&source, &model, 30, &mut band),
        Err(Error::InvalidArgument(_))
    ));
    // A buffer of the wrong size is a caller error. An empty image is not a photo.
    let short = Image {
        width,
        height,
        channels: 3,
        data: &pixels[..10],
    };
    assert!(matches!(
        stitch_photo(&short, &model),
        Err(Error::InvalidArgument(_))
    ));
    let empty = Image {
        width: 0,
        height: 0,
        channels: 3,
        data: &[],
    };
    assert!(matches!(
        stitch_photo(&empty, &model),
        Err(Error::Unsupported(_))
    ));
    assert!(matches!(
        stitch_photo(
            &Image {
                width: 3,
                height: 3,
                channels: 3,
                data: &[0; 27]
            },
            &model
        ),
        Err(Error::Unsupported(_))
    ));
}

#[test]
fn camera_clock_gives_a_zone_only_when_it_lands_on_one() {
    let utc = wall_clock_seconds("20260821025307").unwrap();
    assert_eq!(camera_utc_offset_minutes("20260821105307", utc), Some(480));
    assert_eq!(camera_utc_offset_minutes("20260821082307", utc), Some(330)); // UTC+5:30
    assert_eq!(camera_utc_offset_minutes("20260821050207", utc), None); // 9 minutes after UTC+2
    assert_eq!(camera_utc_offset_minutes("20260820125307", utc), None); // -14:00 is not a time zone
    assert_eq!(wall_clock_seconds("19700101000000"), Some(0));
    assert_eq!(wall_clock_seconds("2026082"), None);
}

#[test]
fn filtfilt_gives_back_a_signal_too_short_to_extend() {
    let filter = butter2_lowpass(0.02, 1000.0);
    assert_eq!(filtfilt2(&filter, &[1.0, 2.0, 3.0]), vec![1.0, 2.0, 3.0]);
}

#[test]
fn few_widely_spaced_samples_do_not_stop_the_levelling() {
    // 4 samples 40 s apart pass the window test (10 s / 40 s + 3 < 4), but the filter needs more than 6.
    let ts = [0.0, 40.0, 80.0, 120.0];
    let acc = [[0.0, 0.0, -1.0]; 4];
    assert!(box_gravity_quats(&ts, &acc, &[[0.0; 3]; 4]).is_none());
}

#[test]
fn zero_gravity_falls_back_instead_of_turning_the_image() {
    let (ts, _, gyr) = turning_camera(12_000);
    assert!(box_gravity_quats(&ts, &vec![[0.0; 3]; 12_000], &gyr).is_none());
}

#[test]
fn video_maps_mark_pixels_outside_each_lens() {
    let bytes = trailer(&[
        record(0x300, &imu_samples(2, 1000, [0, 0, -1024], 1_000_000)),
        metadata(&[field_bytes(5, OFFSET.as_bytes())]),
    ]);
    let t = parse_trailer(&bytes).unwrap();
    let model = LensModel::new(&t, 64, orientation(&t).unwrap()).unwrap();
    let maps = video_maps(&model, 64, 64).unwrap();
    assert_eq!((maps.width, maps.height), (64, 32));
    for plane in [&maps.x[0], &maps.y[0], &maps.x[1], &maps.y[1]] {
        assert_eq!(plane.len(), 64 * 32);
        assert!(plane.iter().all(|&v| v < 64 || v == 65535));
    }
    // Each pixel comes from at least one lens, and both lenses cover part of the sphere.
    for i in 0..64 * 32 {
        assert!(maps.x[0][i] != 65535 || maps.x[1][i] != 65535, "pixel {i}");
    }
    assert!(maps.mask.contains(&255) && maps.mask.contains(&0));
    for (width, lens_height) in [(63, 64), (0, 64), (65536, 64), (64, 70_000)] {
        assert!(matches!(
            video_maps(&model, width, lens_height),
            Err(Error::Unsupported(_))
        ));
    }
}

#[test]
fn refuses_a_trailer_with_too_many_records() {
    // 300 empty records, then the metadata: a real trailer has fewer than 20 records.
    let mut records: Vec<Vec<u8>> = (0..300).map(|_| record(0x7, &[])).collect();
    records.insert(0, metadata(&[field_bytes(5, OFFSET.as_bytes())]));
    assert!(matches!(
        parse_trailer(&trailer(&records)),
        Err(Error::Unsupported(_))
    ));
}

#[test]
fn a_first_frame_far_after_the_imu_start_does_not_decode_the_full_stream() {
    // The first frame says 1 hour, but the IMU starts at 1 s. The reader limits the delay to 10 minutes.
    let meta = metadata(&[
        field_bytes(5, OFFSET.as_bytes()),
        field_varint(24, 3_600_000_000),
    ]);
    let imu = imu_samples(80_000, 10_000, [0, 0, -1024], 1_000_000); // 800 s at 100 Hz
    let t = parse_trailer(&trailer(&[record(0x300, &imu), meta])).unwrap();
    assert!(*t.imu.unwrap().ts.last().unwrap() <= 1_000_000.0 + 600e6 + 120e6);
}
