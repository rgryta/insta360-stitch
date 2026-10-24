mod common;

use common::*;
use insta360_stitch::math::{apply, qrotate, transpose, Vec3, IDENTITY};
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
    // Lens streams of 64x64, output twice as wide as one lens.
    let model = LensModel::new(&t, 64, orientation(&t).unwrap()).unwrap();
    let maps = video_maps(&model, 128, 64, 64).unwrap();
    assert_eq!((maps.width, maps.height), (128, 64));
    for plane in [&maps.x[0], &maps.y[0], &maps.x[1], &maps.y[1]] {
        assert_eq!(plane.len(), 128 * 64);
        assert!(plane.iter().all(|&v| v < 64 || v == 65535));
    }
    // Each pixel comes from at least one lens, and both lenses cover part of the sphere.
    for i in 0..128 * 64 {
        assert!(maps.x[0][i] != 65535 || maps.x[1][i] != 65535, "pixel {i}");
    }
    assert!(maps.mask.contains(&255) && maps.mask.contains(&0));
    // The bands give the same planes as the full maps.
    let mut mask = Vec::new();
    video_maps_rows(&model, 128, 64, 64, 10, |first_row, band| {
        assert_eq!(first_row * 128, mask.len());
        mask.extend_from_slice(&band.mask);
        Ok(())
    })
    .unwrap();
    assert_eq!(mask, maps.mask);
    // A lens stream that is not square, or that the u16 maps cannot hold, is unsupported.
    // A wrong output width is a caller error: yuv420p needs an even height.
    for (lens_width, lens_height) in [(0, 64), (65536, 65536), (64, 70_000), (64, 32)] {
        assert!(matches!(
            video_maps(&model, 128, lens_width, lens_height),
            Err(Error::Unsupported(_))
        ));
    }
    for width in [0, 126, 127, MAX_VIDEO_WIDTH + 4] {
        assert!(matches!(
            video_maps(&model, width, 64, 64),
            Err(Error::InvalidArgument(_))
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

#[test]
fn the_stitcher_checks_all_arguments_before_it_allocates() {
    let bytes = trailer(&[
        record(0x300, &imu_samples(2, 1000, [0, 0, -1024], 1_000_000)),
        metadata(&[field_bytes(5, OFFSET.as_bytes())]),
    ]);
    let stitcher = Stitcher::new(parse_trailer(&bytes).unwrap());
    assert_eq!(stitcher.unsupported_reason(), None);
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
    let whole = stitcher.photo(&source, None).unwrap();
    assert_eq!(
        whole,
        stitch_photo(&source, &stitcher.model(height).unwrap()).unwrap()
    );
    let band = stitcher.photo(&source, Some((10, 7))).unwrap();
    assert_eq!(band, whole[10 * width * 3..17 * width * 3]);
    for band in [(30, 7), (usize::MAX, 2), (1, usize::MAX)] {
        assert!(matches!(
            stitcher.photo(&source, Some(band)),
            Err(Error::InvalidArgument(_))
        ));
    }
    // Dimensions that do not agree with a small buffer: an error, not an allocation of rows * width * channels.
    let huge = Image {
        width: 1 << 40,
        height: 1 << 39,
        channels: 4,
        data: &pixels[..10],
    };
    assert!(matches!(
        stitcher.photo(&huge, Some((0, 1 << 39))),
        Err(Error::InvalidArgument(_))
    ));
    // width * height * channels wraps to 0, the length of an empty buffer.
    let wraps = Image {
        width: 1 << 63,
        height: 1 << 62,
        channels: 2,
        data: &[],
    };
    assert!(matches!(
        stitcher.photo(&wraps, None),
        Err(Error::InvalidArgument(_))
    ));
    let model = stitcher.model(height).unwrap();
    let mut row = vec![0; width * 3];
    assert!(matches!(
        stitch_photo_band(&source, &model, usize::MAX, &mut row),
        Err(Error::InvalidArgument(_))
    ));
    // The video maps are two times as wide as one lens stream by default.
    let maps = stitcher.video_maps(64, 64, None).unwrap();
    assert_eq!((maps.width, maps.height), (128, 64));
    assert_eq!(maps.le_bytes()[0].len(), 128 * 64 * 2);
}

/// Values of the model of the first calibration (files without v6), which agreed with Insta360 Studio exports.
/// Each row: (output row, output column, [u0, v0, u1, v1], weight of lens 0). NaN is outside that lens.
type LensSample = (usize, usize, [f32; 4], f32);
const LENS_SAMPLES: [LensSample; 11] = [
    (
        8,
        31,
        [20.50506, 5.4909563, 43.336323, 6.195568],
        0.35584414,
    ),
    (8, 40, [22.594786, 8.121484, 43.59655, 2.9753523], 1.0),
    (8, 64, [32.423763, 11.085268, f32::NAN, f32::NAN], 1.0),
    (
        8,
        96,
        [43.89181, 5.5107875, 20.666086, 6.3322287],
        0.35584414,
    ),
    (
        32,
        31,
        [3.2752972, 32.549328, 60.18419, 32.423157],
        0.20673192,
    ),
    (32, 90, [55.559723, 32.439644, f32::NAN, f32::NAN], 1.0),
    (
        32,
        95,
        [60.186085, 32.612736, 3.241161, 32.644382],
        0.7932681,
    ),
    (32, 110, [f32::NAN, f32::NAN, 17.178373, 32.628265], 0.0),
    (56, 20, [f32::NAN, f32::NAN, 39.99178, 55.790222], 0.0),
    (
        56,
        32,
        [21.994057, 58.071304, 42.628593, 58.830456],
        0.84576356,
    ),
    (56, 90, [41.349243, 56.723038, 21.558067, 60.42439], 1.0),
];
/// The same, with the premium lens guard (offset state 5).
const GUARD_SAMPLES: [LensSample; 3] = [
    (
        8,
        31,
        [20.229656, 4.866656, 43.602318, 5.5891066],
        0.35584414,
    ),
    (32, 90, [56.113533, 32.452812, f32::NAN, f32::NAN], 1.0),
    (
        56,
        32,
        [21.753876, 58.690784, 42.87785, 59.469067],
        0.84576356,
    ),
];

#[test]
fn the_lens_model_keeps_the_values_that_agree_with_studio() {
    // A change of the lens polynomial, the lens centers, the field of view or the seam blend changes these values.
    let mut maps = RowMaps::new(128);
    let cases = [(0, &LENS_SAMPLES[..]), (5, &GUARD_SAMPLES[..])];
    for (offset_state, &(y, x, want, weight)) in cases
        .iter()
        .flat_map(|(state, samples)| samples.iter().map(move |sample| (*state, sample)))
    {
        let meta = metadata(&[
            field_bytes(5, OFFSET.as_bytes()),
            field_varint(68, offset_state),
        ]);
        let model =
            LensModel::new(&parse_trailer(&trailer(&[meta])).unwrap(), 64, IDENTITY).unwrap();
        model.row(y, 128, 64, &mut maps);
        let got = [maps.u[0][x], maps.v[0][x], maps.u[1][x], maps.v[1][x]];
        for (g, w) in got.iter().zip(want) {
            assert!(
                (w.is_nan() && g.is_nan()) || (g - w).abs() < 0.01,
                "row {y}, column {x}: {got:?} is not {want:?}"
            );
        }
        assert!(
            (maps.weight0[x] - weight).abs() < 1e-4,
            "row {y}, column {x}: weight {}",
            maps.weight0[x]
        );
    }
}

fn v6_trailer(v6: &str) -> Trailer {
    v6_trailer_with(v6, 0, &[])
}

/// A trailer with the v6 calibration, an offset state and more metadata fields.
fn v6_trailer_with(v6: &str, offset_state: u64, more: &[Vec<u8>]) -> Trailer {
    let mut fields = vec![
        field_bytes(5, OFFSET.as_bytes()),
        field_varint(68, offset_state),
        field_bytes(111, v6.as_bytes()),
    ];
    fields.extend_from_slice(more);
    parse_trailer(&trailer(&[metadata(&fields)])).unwrap()
}

/// `window_crop_info` (field 27): source size, image size and offset.
fn crop_field(values: [u64; 6]) -> Vec<u8> {
    let crop: Vec<u8> = (1..=6)
        .zip(values)
        .flat_map(|(k, v)| field_varint(k, v))
        .collect();
    field_bytes(27, &crop)
}

/// The crop of a video: 5312 of the 5376 calibration pixels.
fn video_crop() -> Vec<u8> {
    crop_field([5376, 5376, 5312, 5312, 0, 0])
}

#[test]
fn reads_the_v6_calibration_and_uses_the_first_calibration_without_it() {
    let t = v6_trailer(OFFSET_V6);
    let v6 = t.offset_v6.expect("v6 calibration");
    assert_eq!(
        (v6[0][0], v6[0][3], v6[1][3], v6[1][24]),
        (2.0, 2702.45, 8069.1, 10752.0)
    );
    // Insta360 uses the rotation of v6 if the file has it, else the rotation of the first calibration.
    assert_eq!(t.lens_angles(0), [0.664, 0.009, 89.927]);
    assert_eq!(t.lens_angles(1), [-1.126, 0.323, 90.198]);
    let v1 = parse_trailer(&trailer(&[metadata(&[field_bytes(5, OFFSET.as_bytes())])])).unwrap();
    assert_eq!(v1.offset_v6, None);
    assert_eq!(v1.lens_angles(0), [0.717, 0.092, 89.898]);
    assert_eq!(v1.lens_angles(1), [-1.108, 0.204, 90.231]);
    // A different version, a missing value or an xi that can divide by zero is not v6.
    for bad in [
        OFFSET_V6.replace("_394240", "_197632"),
        OFFSET_V6.replace("_0.00543739_", "_"),
        OFFSET_V6.replacen("2_2.000000_", "2_0.400000_", 1),
        OFFSET_V6.replacen("_4263.350_", "_0_", 1),
    ] {
        assert_eq!(v6_trailer(&bad).offset_v6, None, "{bad}");
    }
}

#[test]
fn the_lens_rotations_and_the_levelling_use_the_v6_angles() {
    // The first calibration with the v6 angles of the lenses.
    let v1_with_v6_angles = OFFSET
        .replace("0.717_0.092_89.898", "0.664_0.009_89.927")
        .replace("-1.108_0.204_90.231", "-1.126_0.323_90.198");
    let imu = record(
        0x300,
        &imu_samples(400, 1000, [3000, -9000, 2000], 1_000_000),
    );
    let with = |fields: Vec<Vec<u8>>| {
        let meta = metadata(&fields);
        parse_trailer(&trailer(&[imu.clone(), meta])).unwrap()
    };
    let v6 = with(vec![
        field_bytes(5, OFFSET.as_bytes()),
        field_bytes(111, OFFSET_V6.as_bytes()),
    ]);
    let same = with(vec![field_bytes(5, v1_with_v6_angles.as_bytes())]);
    let v1 = with(vec![field_bytes(5, OFFSET.as_bytes())]);
    let level = orientation(&v6).unwrap();
    assert_eq!(level, orientation(&same).unwrap());
    assert_ne!(level, orientation(&v1).unwrap());
    let (a, b) = (
        LensModel::new(&v6, 100, level).unwrap(),
        LensModel::new(&same, 100, level).unwrap(),
    );
    for l in 0..2 {
        assert_eq!(a.lenses[l].rotation, b.lenses[l].rotation, "lens {l}");
    }
}

#[test]
fn the_mei_distortion_uses_each_term_as_insta360() {
    let mei = Mei {
        xi: 2.0,
        fx: 1.0,
        fy: 1.0,
        distortion: [
            0.11, -0.07, 0.05, -0.03, 0.02, 0.013, -0.017, 0.019, -0.023, 0.029, -0.031, 0.037,
            -0.041,
        ],
    };
    let (xd, yd) = mei.distort(0.3, -0.2);
    assert!((xd - 0.315555507286).abs() < 1e-11, "{xd}");
    assert!((yd + 0.213421004857).abs() < 1e-11, "{yd}");
}

#[test]
fn the_v6_model_projects_rays_as_insta360() {
    // A lens stream of 2688 pixels: Insta360 scales by (2688 - 1) / (5376 - 1).
    let model = LensModel::new(&v6_trailer(OFFSET_V6), 2688, IDENTITY).unwrap();
    // Positions from an independent implementation of the formula, for lens rays at (angle from the axis,
    // azimuth): the lens centre, (60, 30) and (95, 200) degrees.
    let cases = [
        [
            (1350.973609, 1338.121001),
            (2019.652825, 1725.575355),
            (159.654270, 906.989322),
        ],
        [
            (1346.299479, 1336.746257),
            (2015.544788, 1723.260784),
            (155.306520, 903.152720),
        ],
    ];
    for (l, want) in cases.iter().enumerate() {
        for (&(u, v), (phi, th)) in want
            .iter()
            .zip([(0.0f64, 0.0f64), (60.0, 30.0), (95.0, 200.0)])
        {
            let (f, t) = (phi.to_radians(), th.to_radians());
            let lens_ray = [f.sin() * t.cos(), f.sin() * t.sin(), f.cos()];
            // Insta360 divides by z + xi |P|: the position does not change with the length of the ray.
            let long = model.ray(&apply(
                &transpose(&model.lenses[l].rotation),
                &lens_ray.map(|c| 3.0 * c),
            ));
            let ray = model.ray(&apply(&transpose(&model.lenses[l].rotation), &lens_ray));
            assert!((long.u[l] - ray.u[l]).abs() < 1e-9 && (long.v[l] - ray.v[l]).abs() < 1e-9);
            assert!(
                (ray.u[l] - u).abs() < 1e-5
                    && (ray.v[l] - v).abs() < 1e-5
                    && (ray.phi[l] - phi).abs() < 1e-9,
                "lens {l} at {phi}/{th}: ({}, {}) is not ({u}, {v})",
                ray.u[l],
                ray.v[l]
            );
        }
    }
    // Insta360 samples a lens up to 120 degrees from its axis with the v6 calibration.
    assert_eq!(model.half_fov, 120.0);
}

#[test]
fn the_premium_guard_changes_the_lens_to_the_guard_design_as_insta360() {
    let raw = LensModel::new(&v6_trailer(OFFSET_V6), 5376, IDENTITY).unwrap();
    let guard = LensModel::new(&v6_trailer_with(OFFSET_V6, 5, &[]), 5376, IDENTITY).unwrap();
    // The focal lengths that Insta360 (`convertOffset`, type 0x35) gives for this calibration. Insta360 writes
    // them with 3 decimals.
    for (l, focal) in [4359.799, 4359.787].into_iter().enumerate() {
        let (r, g) = (raw.lenses[l], guard.lenses[l]);
        let (rm, gm) = (r.mei.unwrap(), g.mei.unwrap());
        assert!(
            (gm.fx - focal).abs() < 1e-3 && (gm.fy - focal).abs() < 1e-3,
            "lens {l}: {gm:?}"
        );
        assert_eq!(gm.xi, 2.0);
        assert_eq!(
            gm.distortion[..5],
            [0.2701227043, 1.213889541, 1.572668844, -10.02050387, 0.0]
        );
        assert_eq!(gm.distortion[5..], rm.distortion[5..]);
        assert_eq!((g.cx, g.cy, g.rotation), (r.cx, r.cy, r.rotation));
    }
}

#[test]
fn a_video_crop_moves_the_lens_centres_as_insta360() {
    // At the image size, the crop does not scale: it moves each centre by (5376 - 5312) / 2.
    let model = LensModel::new(
        &v6_trailer_with(OFFSET_V6, 0, &[video_crop()]),
        5312,
        IDENTITY,
    )
    .unwrap();
    let raw = LensModel::new(&v6_trailer(OFFSET_V6), 5376, IDENTITY).unwrap();
    let want = [(2670.45, 2644.74), (7973.1 - 5312.0, 2641.99)];
    for (l, (cx, cy)) in want.into_iter().enumerate() {
        let lens = model.lenses[l];
        assert!(
            (lens.cx - cx).abs() < 1e-9 && (lens.cy - cy).abs() < 1e-9,
            "lens {l}: {lens:?}"
        );
        assert_eq!(lens.mei.unwrap().fx, raw.lenses[l].mei.unwrap().fx);
    }
    // An offset moves the centres, a smaller source scales them, and a source with a different shape is ignored.
    let lens0 = |crop: [u64; 6]| {
        LensModel::new(
            &v6_trailer_with(OFFSET_V6, 0, &[crop_field(crop)]),
            100,
            IDENTITY,
        )
        .unwrap()
        .lenses[0]
    };
    let s = 99.0 / 5311.0;
    let moved = lens0([5376, 5376, 5312, 5312, 10, 4]);
    assert!(
        (moved.cx - (2702.45 - 32.0 + 10.0) * s).abs() < 1e-9,
        "{moved:?}"
    );
    assert!(
        (moved.cy - (2676.74 - 32.0 + 4.0) * s).abs() < 1e-9,
        "{moved:?}"
    );
    let half = lens0([2688, 2688, 2656, 2656, 0, 0]);
    let s = 99.0 / 2655.0;
    assert!(
        (half.cx - (0.5 * (2702.45 + 0.5) - 0.5 - 16.0) * s).abs() < 1e-9,
        "{half:?}"
    );
    assert!(
        (half.mei.unwrap().fx - 0.5 * 4263.35 * s).abs() < 1e-9,
        "{half:?}"
    );
    let other = lens0([5376, 2688, 5312, 5312, 0, 0]);
    assert!(
        (other.cx - 2702.45 * 99.0 / 5375.0).abs() < 1e-9,
        "{other:?}"
    );

    // The lens stream has 3840 pixels for the 5312 pixels of the crop.
    let stream = LensModel::new(
        &v6_trailer_with(OFFSET_V6, 0, &[video_crop()]),
        3840,
        IDENTITY,
    )
    .unwrap();
    let s = 3839.0 / 5311.0;
    assert!((stream.lenses[0].cx - 2670.45 * s).abs() < 1e-9);
    assert!((stream.lenses[0].mei.unwrap().fx - 4263.35 * s).abs() < 1e-9);
}
