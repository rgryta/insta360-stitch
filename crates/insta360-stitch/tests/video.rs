mod common;

use common::*;
use insta360_stitch::math::{Mat3, IDENTITY};
use insta360_stitch::*;

/// A smooth test pattern: the bilinear sampling then shows the geometry, not the aliasing of noise.
fn smooth(x: f64, y: f64, phase: f64) -> u8 {
    (128.0 + 90.0 * ((x * 0.21 + phase).sin() * (y * 0.17).cos())) as u8
}

fn pattern(size: usize, phase: f64) -> Vec<u8> {
    (0..size * size)
        .map(|i| smooth((i % size) as f64, (i / size) as f64, phase))
        .collect()
}

/// A pattern of lower frequency for the colour test: a half-pixel shift then gives a larger error than the
/// bilinear sampling. `chroma` samples it at the chroma positions of yuv420p with left siting: luma column 2x,
/// luma row 2y + 0.5.
fn slow_pattern(size: usize, phase: f64, chroma: bool) -> Vec<u8> {
    let slow = |x: f64, y: f64| {
        (128.0 + 100.0 * ((x * 0.1 + phase).sin() * (y * 0.08 + phase).cos())) as u8
    };
    if !chroma {
        return (0..size * size)
            .map(|i| slow((i % size) as f64, (i / size) as f64))
            .collect();
    }
    let half = size / 2;
    (0..half * half)
        .map(|i| slow(2.0 * (i % half) as f64, 2.0 * (i / half) as f64 + 0.5))
        .collect()
}

fn stitcher() -> Stitcher {
    Stitcher::new(
        parse_trailer(&trailer(&[
            record(0x300, &imu_samples(2, 1000, [0, 0, -1024], 1_000_000)),
            metadata(&[field_bytes(5, OFFSET.as_bytes())]),
        ]))
        .unwrap(),
    )
}

#[test]
fn a_frame_is_the_same_as_the_photo_stitch_with_the_same_rotation() {
    let size = 64;
    // A tilted rotation that is not symmetric checks the order of the rotation.
    let t = parse_trailer(&trailer(&[metadata(&[
        field_bytes(5, OFFSET.as_bytes()),
        field_varint(68, 5),
    ])]))
    .unwrap();
    let rotation = math::rotate_matrix(12.0, -25.0, 40.0);
    assert_ne!(rotation, math::transpose(&rotation));
    let (lens0, lens1) = (pattern(size, 0.0), pattern(size, 1.3));
    // The photo layout: the two lens crops side by side, one channel.
    let mut photo = vec![0u8; 2 * size * size];
    for y in 0..size {
        photo[y * 2 * size..y * 2 * size + size].copy_from_slice(&lens0[y * size..][..size]);
        photo[y * 2 * size + size..(y + 1) * 2 * size].copy_from_slice(&lens1[y * size..][..size]);
    }
    let source = Image {
        width: 2 * size,
        height: size,
        channels: 1,
        data: &photo,
    };
    // A narrow field of view (92 degrees) puts the edge of each lens inside the seam blend (81-99 degrees).
    // Thus, a stitch that does not check where each lens sees the ray gives a different image.
    let narrow = |level| LensModel {
        half_fov: 92.0,
        ..LensModel::new(&t, size, level).unwrap()
    };
    let want = stitch_photo(&source, &narrow(rotation)).unwrap();

    // The video layout: yuv420p, lens 0 on top of lens 1. The chroma is constant.
    let frames = FrameStitcher::new(&narrow(IDENTITY), size, 2 * size).unwrap();
    let mut src = [lens0, lens1].concat();
    src.resize(frames.input_frame_bytes(), 128);
    let mut out = vec![0u8; frames.output_frame_bytes()];
    frames.stitch_yuv420(&rotation, &src, &mut out).unwrap();
    let luma = &out[..want.len()];
    let mae = luma
        .iter()
        .zip(&want)
        .map(|(&a, &b)| (a as f64 - b as f64).abs())
        .sum::<f64>()
        / want.len() as f64;
    // Single pixels differ more at the edge of a lens crop (the photo reads black outside, the video repeats
    // the edge). The 99.9th percentile shows a wrong seam: it is 3 levels here, 10 without the lens edge check.
    let mut diffs: Vec<u8> = luma
        .iter()
        .zip(&want)
        .map(|(&a, &b)| a.abs_diff(b))
        .collect();
    diffs.sort_unstable();
    let p999 = diffs[diffs.len() * 999 / 1000];
    assert!(p999 <= 5, "99.9th percentile difference {p999} levels");
    assert!(mae < 1.0, "mean difference {mae:.3} levels");
    assert!(out[want.len()..].iter().all(|&c| c == 128));
}

#[test]
fn the_colour_planes_follow_the_geometry_of_the_luma() {
    let size = 64;
    let frames = stitcher().frame_stitcher(size, None).unwrap();
    let rotation = math::rotate_matrix(12.0, -25.0, 40.0);
    // Lens 0 and lens 1 of luma, Cb and Cr: each plane has a different pattern.
    let stitch = |[y0, y1, b0, b1, r0, r1]: [f64; 6]| {
        let src = [
            slow_pattern(size, y0, false),
            slow_pattern(size, y1, false),
            slow_pattern(size, b0, true),
            slow_pattern(size, b1, true),
            slow_pattern(size, r0, true),
            slow_pattern(size, r1, true),
        ]
        .concat();
        let mut out = vec![0u8; frames.output_frame_bytes()];
        frames.stitch_yuv420(&rotation, &src, &mut out).unwrap();
        out
    };
    let colour = stitch([0.0, 1.3, 0.0, 1.3, 2.1, 0.6]);
    // A luma stitch of the Cr patterns gives the reference of Cr.
    let cr_luma = stitch([2.1, 0.6, 0.0, 0.0, 0.0, 0.0]);
    let (w, h) = (frames.width(), frames.height());
    // Output chroma (x, y) is at luma column 2x, between luma rows 2y and 2y + 1.
    let error = |plane: &[u8], luma: &[u8]| {
        let mut sum = 0.0;
        for y in 0..h / 2 {
            for x in 0..w / 2 {
                let want =
                    (luma[2 * y * w + 2 * x] as f64 + luma[(2 * y + 1) * w + 2 * x] as f64) / 2.0;
                sum += (plane[y * w / 2 + x] as f64 - want).abs();
            }
        }
        sum / (w * h / 4) as f64
    };
    let (cb, cr) = colour[w * h..].split_at(w * h / 4);
    let (cb_error, cr_error) = (error(cb, &colour[..w * h]), error(cr, &cr_luma[..w * h]));
    // It is 0.5 levels. A chroma position that is wrong by a half luma pixel gives 1.5 levels or more.
    assert!(
        cb_error < 1.0 && cr_error < 1.0,
        "Cb {cb_error:.2}, Cr {cr_error:.2} levels"
    );
}

#[test]
fn the_frame_stitcher_refuses_wrong_arguments() {
    let stitcher = stitcher();
    for (lens, width) in [(63, None), (64, Some(130)), (64, Some(MAX_VIDEO_WIDTH + 4))] {
        assert!(
            stitcher.frame_stitcher(lens, width).is_err(),
            "{lens} {width:?}"
        );
    }
    // A model with a level rotation would turn each frame two times.
    let levelled = stitcher.model(64).unwrap();
    assert!(matches!(
        FrameStitcher::new(&levelled, 64, 128),
        Err(Error::InvalidArgument(_))
    ));
    let frames = stitcher.frame_stitcher(64, None).unwrap();
    let mut out = vec![0u8; frames.output_frame_bytes()];
    let src = vec![0u8; frames.input_frame_bytes()];
    assert!(matches!(
        frames.stitch_yuv420(&IDENTITY, &src[1..], &mut out),
        Err(Error::InvalidArgument(_))
    ));
    let nan: Mat3 = [[f64::NAN; 3]; 3];
    assert!(matches!(
        frames.stitch_yuv420(&nan, &src, &mut out),
        Err(Error::InvalidArgument(_))
    ));
}

/// Raw IMU samples (1 kHz) of a camera that stands upright and turns about the vertical IMU axis.
/// `yaw_rate(t)` is in degrees per second. The gravity stays on the same axis (8 g range, 2000 deg/s range).
fn turning_imu(seconds: f64, yaw_rate: impl Fn(f64) -> f64) -> Imu {
    let mut imu = Imu::default();
    let n = (seconds * 1000.0) as usize;
    for i in 0..n {
        let t = i as f64 / 1000.0;
        imu.ts.push(1_000_000.0 + t * 1e6);
        let raw = |v: f64, range: f64| (32768.0 + v / range * 32768.0).round() as u16;
        imu.raw.extend([
            raw(0.0, 8.0),
            raw(0.0, 8.0),
            raw(-1.0, 8.0),
            raw(0.0, 2000.0),
            raw(0.0, 2000.0),
            raw(yaw_rate(t), 2000.0),
        ]);
    }
    imu
}

fn angle_between(a: &Mat3, b: &Mat3) -> f64 {
    let m = math::mul(a, &math::transpose(b));
    ((m[0][0] + m[1][1] + m[2][2] - 1.0) / 2.0)
        .clamp(-1.0, 1.0)
        .acos()
        .to_degrees()
}

/// The frame times: 24 fps from 12 s to 18 s (after the 10 s gravity window).
fn frame_times() -> Vec<f64> {
    (0..145)
        .map(|i| 13_000_000.0 + i as f64 * 1e6 / 24.0)
        .collect()
}

fn trailer_for(imu_seconds: f64) -> Trailer {
    let mut t = parse_trailer(&trailer(&[
        record(0x300, &imu_samples(2, 1000, [0, 0, -1024], 1_000_000)),
        metadata(&[
            field_bytes(5, OFFSET.as_bytes()),
            field_varint(24, 13_000_000),
        ]),
    ]))
    .unwrap();
    t.acc_range = 8.0;
    t.gyro_range = 2000.0;
    t.imu = Some(turning_imu(imu_seconds, |_| 0.0));
    t
}

#[test]
fn lock_turns_each_frame_against_the_camera_and_off_keeps_the_first_frame() {
    let t = trailer_for(20.0);
    // 15 degrees per second for 6 s: 90 degrees.
    let imu = turning_imu(20.0, |_| 15.0);
    let times = frame_times();
    let lock = frame_orientations(&t, &imu, &times, 24.0, Stabilization::Lock).unwrap();
    let turned = angle_between(&lock[144], &lock[0]);
    assert!(
        (turned - 90.0).abs() < 1.0,
        "lock turned {turned:.2} degrees"
    );
    let half = angle_between(&lock[72], &lock[0]);
    assert!(
        (half - 45.0).abs() < 1.0,
        "lock turned {half:.2} degrees at half time"
    );
    // The horizon stays level: the output up axis stays the camera up axis for an upright camera.
    let up = |m: &Mat3| math::apply(m, &[0.0, 1.0, 0.0]);
    let tilt = math::dot(&up(&lock[0]), &up(&lock[144]))
        .clamp(-1.0, 1.0)
        .acos()
        .to_degrees();
    assert!(tilt < 0.5, "the horizon moved {tilt:.2} degrees");
    // The direction of the turn: the angles above are the same for a turn in the wrong direction.
    // This direction agrees with Insta360 Studio on real clips.
    let (s, c) = 90f64.to_radians().sin_cos();
    let turn: Mat3 = [[c, 0.0, s], [0.0, 1.0, 0.0], [-s, 0.0, c]];
    let off_turn = angle_between(&lock[144], &math::mul(&lock[0], &turn));
    assert!(
        off_turn < 1.0,
        "lock is {off_turn:.2} degrees from the turn of the camera"
    );

    let off = frame_orientations(&t, &imu, &times, 24.0, Stabilization::Off).unwrap();
    assert!(off.iter().all(|m| m == &off[0]));
    assert_eq!(off[0], orientation(&t).unwrap());
}

#[test]
fn follow_keeps_a_slow_turn_and_removes_a_fast_shake() {
    let t = trailer_for(20.0);
    let times = frame_times();
    // A slow turn: 10 degrees per second.
    let slow = turning_imu(20.0, |_| 10.0);
    let follow = frame_orientations(&t, &slow, &times, 24.0, Stabilization::Follow).unwrap();
    let kept = angle_between(&follow[100], &follow[0]);
    assert!(
        kept < 3.0,
        "follow did not follow the slow turn: {kept:.2} degrees"
    );
    // A fast shake: +-3 degrees at 4 Hz, no net turn. Follow must hold the view like lock.
    let shake = turning_imu(20.0, |t| {
        3.0 * 2.0 * std::f64::consts::PI * 4.0 * (2.0 * std::f64::consts::PI * 4.0 * t).cos()
    });
    let follow = frame_orientations(&t, &shake, &times, 24.0, Stabilization::Follow).unwrap();
    let lock = frame_orientations(&t, &shake, &times, 24.0, Stabilization::Lock).unwrap();
    let worst = follow
        .iter()
        .zip(&lock)
        .map(|(f, l)| angle_between(f, l))
        .fold(0.0, f64::max);
    assert!(worst < 0.5, "follow kept {worst:.2} degrees of the shake");
}

#[test]
fn frame_times_use_the_last_entries_of_the_exposure_table() {
    let mut table = Vec::new();
    for i in 0..10u64 {
        table.extend((5_000_000 + i * 40_000).to_le_bytes());
        table.extend(0.01f64.to_le_bytes());
    }
    let t = parse_trailer(&trailer(&[
        record(0x300, &imu_samples(2, 1000, [0, 0, -1024], 1_000_000)),
        record(0x400, &table),
        metadata(&[
            field_bytes(5, OFFSET.as_bytes()),
            field_varint(24, 9_000_000),
        ]),
    ]))
    .unwrap();
    assert_eq!(t.exposures.as_ref().map(Vec::len), Some(10));
    // The table must have an entry for each frame.
    assert!(uses_exposure_table(&t, 10));
    assert!(!uses_exposure_table(&t, 11));
    assert!(!uses_exposure_table(&t, 0));
    // 7 frames: the first frame at the first-frame time (9 s), the spacing from the last 7 entries.
    let mut uneven = table.clone();
    // Make the spacing of the last entries uneven: entry 8 is 10 ms late.
    uneven[8 * 16..8 * 16 + 8].copy_from_slice(&(5_000_000u64 + 8 * 40_000 + 10_000).to_le_bytes());
    let t2 = parse_trailer(&trailer(&[
        record(0x300, &imu_samples(2, 1000, [0, 0, -1024], 1_000_000)),
        record(0x400, &uneven),
        metadata(&[
            field_bytes(5, OFFSET.as_bytes()),
            field_varint(24, 9_000_000),
        ]),
    ]))
    .unwrap();
    let times = frame_times_us(&t2, 7, 25.0);
    assert_eq!(times[0], 9_000_000.0);
    assert_eq!(times[5], 9_000_000.0 + 5.0 * 40_000.0 + 10_000.0);
    assert_eq!(times[6], 9_000_000.0 + 6.0 * 40_000.0);
    // A video with more frames than the table: the first-frame time and the frame rate.
    let times = frame_times_us(&t, 12, 25.0);
    assert_eq!((times[0], times[1]), (9_000_000.0, 9_040_000.0));
    // The frame times do not use the durations: a table with a bad duration is used.
    let mut nan = table.clone();
    nan[3 * 16 + 8..4 * 16].copy_from_slice(&f64::NAN.to_le_bytes());
    let t3 = parse_trailer(&trailer(&[
        record(0x400, &nan),
        metadata(&[field_bytes(5, OFFSET.as_bytes())]),
    ]))
    .unwrap();
    assert_eq!(t3.exposures.as_ref().map(Vec::len), Some(10));
    // A table with a time that does not increase is not used.
    let mut bad = table.clone();
    bad[16..24].copy_from_slice(&1u64.to_le_bytes());
    let t = parse_trailer(&trailer(&[
        record(0x400, &bad),
        metadata(&[field_bytes(5, OFFSET.as_bytes())]),
    ]))
    .unwrap();
    assert!(t.exposures.is_none());
}

#[test]
fn the_full_imu_reader_reads_past_two_minutes_and_averages_to_250_hz() {
    // 200 s at 1 kHz, the first frame at 1 s: the trailer reader keeps only about 121 s.
    let samples = imu_samples(200_000, 1000, [0, 0, -1024], 1_000_000);
    let bytes = trailer(&[
        record(0x300, &samples),
        metadata(&[
            field_bytes(5, OFFSET.as_bytes()),
            field_varint(24, 1_000_000),
        ]),
    ]);
    let t = parse_trailer(&bytes).unwrap();
    assert!(t.imu.as_ref().unwrap().len() < 125_000);
    let full = read_imu(&mut std::io::Cursor::new(&bytes), &t).unwrap();
    // 1 kHz averaged in groups of 4.
    assert_eq!(full.len(), 50_000);
    assert_eq!(full.ts[0], 1_000_000.0 + 1500.0);
    assert!(*full.ts.last().unwrap() > 200_000_000.0);
    assert_eq!(
        &full.raw[..6],
        &[32768, 32768, 32768 - 1024, 32768, 32768, 32768]
    );
}

#[test]
fn lock_and_follow_refuse_an_imu_stream_that_is_too_short() {
    let t = trailer_for(20.0);
    let imu = turning_imu(0.005, |_| 15.0);
    for mode in [Stabilization::Lock, Stabilization::Follow] {
        assert!(matches!(
            frame_orientations(&t, &imu, &frame_times(), 24.0, mode),
            Err(Error::Unsupported(_))
        ));
    }
    assert!(frame_orientations(&t, &imu, &frame_times(), 24.0, Stabilization::Off).is_ok());
}

#[test]
fn the_full_imu_reader_averages_without_a_rounding_bias() {
    // 1 kHz, thus groups of 4. In each group, the gyroscope z value is 0, 1, 1, 1 (mean 0.75) and the gyroscope x
    // value is 0, 0, 0, 1 (mean 0.25). Each channel has its own remainder.
    let mut samples = imu_samples(4000, 1000, [0, 0, -1024], 1_000_000);
    for i in 0..4000 {
        if i % 4 >= 1 {
            samples[i * 20 + 18..i * 20 + 20].copy_from_slice(&32769u16.to_le_bytes());
        }
        if i % 4 == 3 {
            samples[i * 20 + 14..i * 20 + 16].copy_from_slice(&32769u16.to_le_bytes());
        }
    }
    let bytes = trailer(&[
        record(0x300, &samples),
        metadata(&[
            field_bytes(5, OFFSET.as_bytes()),
            field_varint(24, 1_000_000),
        ]),
    ]);
    let full = read_imu(
        &mut std::io::Cursor::new(&bytes),
        &parse_trailer(&bytes).unwrap(),
    )
    .unwrap();
    let mean = |k: usize| {
        full.raw
            .chunks(6)
            .map(|s| s[k] as f64 - 32768.0)
            .sum::<f64>()
            / full.len() as f64
    };
    assert!(
        (mean(5) - 0.75).abs() < 0.01,
        "gyroscope z mean {}",
        mean(5)
    );
    assert!(
        (mean(3) - 0.25).abs() < 0.01,
        "gyroscope x mean {}",
        mean(3)
    );
}

#[test]
fn lock_also_stabilizes_a_short_clip() {
    // 4 s of IMU: shorter than the 10 s gravity window of a photo. 20 degrees per second.
    let t = trailer_for(4.0);
    let imu = turning_imu(4.0, |_| 20.0);
    let times: Vec<f64> = (0..48)
        .map(|i| 2_000_000.0 + i as f64 * 1e6 / 24.0)
        .collect();
    let lock = frame_orientations(&t, &imu, &times, 24.0, Stabilization::Lock).unwrap();
    let turned = angle_between(&lock[47], &lock[0]);
    assert!(
        (turned - 47.0 / 24.0 * 20.0).abs() < 1.0,
        "lock turned {turned:.2} degrees"
    );
}
