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

fn frame_matches_photo(size: usize) {
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
    // The photo and the frame move the seam by the flow of their lens images (82-98 degrees from the axis of lens
    // 0). The two lens patterns do not show the same scene: a small difference of the strips (the photo reads black
    // outside a lens crop, the video repeats the edge) can change that flow. At 64 pixels, the two flows agree,
    // and the test compares all pixels. At 960 pixels, it compares the pixels outside the seam zone, and checks
    // where the flow changes the frame.
    // (The unit tests of `video` check the lens edges.)
    let model = |level| LensModel::new(&t, size, level).unwrap();
    let want = stitch_photo(&source, &model(rotation)).unwrap();

    // The video layout: yuv420p, lens 0 on top of lens 1. The chroma is constant.
    let frames = FrameStitcher::new(&model(IDENTITY), size, 2 * size).unwrap();
    let mut src = [lens0, lens1].concat();
    src.resize(frames.input_frame_bytes(), 128);
    let mut out = vec![0u8; frames.output_frame_bytes()];
    let flow = frames
        .frame_flow(&src)
        .unwrap()
        .expect("a model with the Studio blend");
    let frame_rotation = FrameRotation::fixed(rotation);
    frames
        .stitch_yuv420_flow(
            &frame_rotation,
            Some(&FrameFlow::mean(&[&flow])),
            &src,
            &mut out,
        )
        .unwrap();
    let mut plain = vec![0u8; frames.output_frame_bytes()];
    frames.stitch_yuv420(&rotation, &src, &mut plain).unwrap();
    let (w, h) = (2 * size, size);
    let photo_model = model(rotation);
    let phi0: Vec<f64> = (0..w * h)
        .map(|i| {
            let (sp, cp) = (std::f64::consts::PI * ((i / w) as f64 + 0.5) / h as f64).sin_cos();
            let (st, ct) = (std::f64::consts::TAU * ((i % w) as f64 + 0.5) / w as f64).sin_cos();
            photo_model.ray(&[-sp * st, cp, sp * ct]).phi[0]
        })
        .collect();
    // The flow changes the pixels in the band, and none far from it (its map is 0 at 12 degrees from the seam).
    let changed = |i: usize| out[i] != plain[i];
    let in_band = (0..w * h).filter(|&i| (82.0..=98.0).contains(&phi0[i]) && changed(i));
    assert!(in_band.count() > w * h / 100);
    assert!((0..w * h).all(|i| (75.0..=105.0).contains(&phi0[i]) || !changed(i)));
    let outside: Vec<bool> = phi0
        .iter()
        .map(|p| size == 64 || !(78.0..=102.0).contains(p))
        .collect();
    let pick = |v: &[u8]| -> Vec<u8> {
        v.iter()
            .zip(&outside)
            .filter(|(_, &o)| o)
            .map(|(&a, _)| a)
            .collect()
    };
    let (luma, want) = (pick(&out[..want.len()]), pick(&want));
    let mae = luma
        .iter()
        .zip(&want)
        .map(|(&a, &b)| (a as f64 - b as f64).abs())
        .sum::<f64>()
        / want.len() as f64;
    // Single pixels differ more at the edge of a lens crop (the photo reads black outside, the video repeats
    // the edge).
    let mut diffs: Vec<u8> = luma
        .iter()
        .zip(&want)
        .map(|(&a, &b)| a.abs_diff(b))
        .collect();
    diffs.sort_unstable();
    let p999 = diffs[diffs.len() * 999 / 1000];
    assert!(p999 <= 5, "99.9th percentile difference {p999} levels");
    assert!(mae < 1.0, "mean difference {mae:.3} levels");
    assert!(out[w * h..].iter().all(|&c| c == 128));
}

#[test]
fn a_frame_is_the_same_as_the_photo_stitch_with_the_same_rotation() {
    // 128 wide: an exact ray for each pixel. 1920 wide: a grid of 2 pixels, interpolated between its points.
    for size in [64, 960] {
        frame_matches_photo(size);
    }
}

fn colour_follows_luma(size: usize) {
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
fn the_colour_planes_follow_the_geometry_of_the_luma() {
    // An exact ray for each pixel (128 wide), and the grid of 2 pixels (1920 wide).
    for size in [64, 960] {
        colour_follows_luma(size);
    }
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
fn lock_removes_the_gyroscope_bias_of_a_still_camera() {
    // A camera on a tripod with a gyroscope bias of 0.5 degree per second. Without the bias removal of
    // Insta360, Lock would turn 3 degrees in the 6 s of frames.
    let t = trailer_for(20.0);
    let imu = turning_imu(20.0, |_| 0.5);
    let lock = frame_orientations(&t, &imu, &frame_times(), 24.0, Stabilization::Lock).unwrap();
    let turned = angle_between(&lock[144], &lock[0]);
    assert!(turned < 0.01, "lock turned {turned:.3} degrees");
    // A stream from `read_imu` has no bias already: the levelling does not look for it again.
    let mut done = imu.clone();
    done.gyro_bias_removed = true;
    let lock = frame_orientations(&t, &done, &frame_times(), 24.0, Stabilization::Lock).unwrap();
    let turned = angle_between(&lock[144], &lock[0]);
    assert!(
        (turned - 2.93).abs() < 0.05,
        "lock turned {turned:.3} degrees"
    );
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
    // A fast shake: +-3 degrees at 4 Hz, no net turn. Away from the ends of the clip (Insta360 does not smooth the
    // first and the last sample), follow turns the lock view by an almost constant heading.
    let shake = turning_imu(20.0, |t| {
        3.0 * 2.0 * std::f64::consts::PI * 4.0 * (2.0 * std::f64::consts::PI * 4.0 * t).cos()
    });
    let follow = frame_orientations(&t, &shake, &times, 24.0, Stabilization::Follow).unwrap();
    let lock = frame_orientations(&t, &shake, &times, 24.0, Stabilization::Lock).unwrap();
    let turn = |k: usize| math::mul(&math::transpose(&lock[k]), &follow[k]);
    let worst = (36..109)
        .map(|k| angle_between(&turn(k), &turn(72)))
        .fold(0.0, f64::max);
    assert!(worst < 0.5, "follow kept {worst:.2} degrees of the shake");
}

#[test]
fn follow_averages_the_heading_over_3_s() {
    // A slow swing: +-5 degrees with a period of 3 s. The centred box filter of 3 s (one period) removes it; the
    // 3-tap passes alone keep approximately 2 degrees.
    let t = trailer_for(20.0);
    let swing = turning_imu(20.0, |t| {
        let w = 2.0 * std::f64::consts::PI / 3.0;
        5.0 * w * (w * t).cos()
    });
    let follow =
        frame_orientations(&t, &swing, &frame_times(), 24.0, Stabilization::Follow).unwrap();
    let lock = frame_orientations(&t, &swing, &frame_times(), 24.0, Stabilization::Lock).unwrap();
    // Follow turns the lock view by the smoothed heading: with the swing removed, that turn follows the swing.
    let turn = |k: usize| math::mul(&math::transpose(&lock[k]), &follow[k]);
    assert!(angle_between(&lock[54], &lock[72]) > 4.0);
    let worst = (40..105)
        .map(|k| angle_between(&turn(k), &turn(72)))
        .fold(0.0, f64::max);
    assert!(
        worst < 0.5,
        "follow kept {worst:.2} degrees of the slow swing"
    );
}

#[test]
fn follow_looks_along_lens_1_and_uses_one_sample_per_frame() {
    // A camera that does not turn: the view centre (output -z) is the axis of lens 1 (camera +z), within the
    // calibration of the lenses.
    let t = trailer_for(20.0);
    let still = turning_imu(20.0, |_| 0.0);
    let follow =
        frame_orientations(&t, &still, &frame_times(), 24.0, Stabilization::Follow).unwrap();
    let centre = math::apply(&follow[72], &[0.0, 0.0, -1.0]);
    assert!(centre[2] > 0.99, "the view looks along {centre:?}");
    // The heading is the one of the ray to the centre of the lens 1 image (pixel 499.5 of 1000), not the one of
    // the lens axis. Both headings in the world frame (the lock view), about its vertical y axis.
    let model = LensModel::new(&t, 1000, math::IDENTITY).unwrap();
    let (mut yaw, mut pitch) = (0.0f64, 0.0f64);
    for _ in 0..50 {
        let dir = |yaw: f64, pitch: f64| {
            [
                yaw.sin() * pitch.cos(),
                pitch.sin(),
                yaw.cos() * pitch.cos(),
            ]
        };
        let r = model.ray(&dir(yaw, pitch));
        let (ry, rp) = (
            model.ray(&dir(yaw + 1e-6, pitch)),
            model.ray(&dir(yaw, pitch + 1e-6)),
        );
        let j = [
            [(ry.u[1] - r.u[1]) / 1e-6, (rp.u[1] - r.u[1]) / 1e-6],
            [(ry.v[1] - r.v[1]) / 1e-6, (rp.v[1] - r.v[1]) / 1e-6],
        ];
        let (du, dv) = (r.u[1] - 499.5, r.v[1] - 499.5);
        let det = j[0][0] * j[1][1] - j[0][1] * j[1][0];
        yaw -= (j[1][1] * du - j[0][1] * dv) / det;
        pitch -= (j[0][0] * dv - j[1][0] * du) / det;
    }
    let lock = frame_orientations(&t, &still, &frame_times(), 24.0, Stabilization::Lock).unwrap();
    let to_world = math::transpose(&lock[72]);
    let ray = [
        yaw.sin() * pitch.cos(),
        pitch.sin(),
        yaw.cos() * pitch.cos(),
    ];
    let (view, lens) = (
        math::apply(&to_world, &centre),
        math::apply(&to_world, &ray),
    );
    let (view, lens) = (view[0].atan2(view[2]), lens[0].atan2(lens[2]));
    assert!(
        (view - lens).abs().to_degrees() < 0.02,
        "view heading {view} is not the lens 1 heading {lens}"
    );
    // The filter has one sample for each frame: with 8 frames, the box window (35 samples) is too long, and only
    // the 3-tap passes smooth. A turn between frames 3 and 4 then stays a smooth step, not a jump.
    let step = turning_imu(20.0, |t| {
        if (12.10..12.18).contains(&t) {
            300.0
        } else {
            0.0
        }
    });
    let few: Vec<f64> = (0..8)
        .map(|i| 13_000_000.0 + i as f64 * 1e6 / 24.0)
        .collect();
    let follow = frame_orientations(&t, &step, &few, 24.0, Stabilization::Follow).unwrap();
    let lock = frame_orientations(&t, &step, &few, 24.0, Stabilization::Lock).unwrap();
    let turn = |k: usize| {
        angle_between(
            &math::mul(&math::transpose(&lock[k]), &follow[k]),
            &math::mul(&math::transpose(&lock[0]), &follow[0]),
        )
    };
    let turns: Vec<f64> = (0..8).map(turn).collect();
    assert!(
        turns[1] > 0.5 && turns[6] > 0.5 && turns[6] < 23.5,
        "{turns:?}"
    );
}

/// An exposure table: entry i ends at 5 s + i * 40 ms + `shift_us`, with the exposure time `duration(i)` (s).
fn exposure_table(count: u64, shift_us: u64, duration: impl Fn(u64) -> f64) -> Vec<u8> {
    let mut table = Vec::new();
    for i in 0..count {
        table.extend((5_000_000 + i * 40_000 + shift_us).to_le_bytes());
        table.extend(duration(i).to_le_bytes());
    }
    table
}

#[test]
fn frame_times_are_the_middle_of_each_exposure_plus_the_gyro_delay() {
    // 10 entries; the first video frame is the entry that ends at the first-frame time (entry 2).
    let table = exposure_table(10, 0, |i| if i == 3 { 0.004 } else { 0.01 });
    let fields = || {
        metadata(&[
            field_bytes(5, OFFSET.as_bytes()),
            field_varint(24, 5_080_000),
            field_f64(25, 21.244),
            field_f64(28, 1.6),
        ])
    };
    let t = parse_trailer(&trailer(&[record(0x400, &table), fields()])).unwrap();
    assert_eq!((t.rolling_shutter_ms, t.gyro_delay_ms), (21.244, 1.6));
    // The table must have an entry for each frame from the first frame.
    assert!(uses_exposure_table(&t, 8));
    assert!(!uses_exposure_table(&t, 9));
    assert!(!uses_exposure_table(&t, 0));
    // The middle of the exposure (end - duration / 2) plus 1.6 ms.
    let times = frame_times_us(&t, 8, 25.0);
    assert_eq!(times.len(), 8);
    assert_eq!(times[0], 5_080_000.0 - 5_000.0 + 1_600.0);
    assert_eq!(times[1], 5_120_000.0 - 2_000.0 + 1_600.0);
    assert_eq!(times[7], 5_360_000.0 - 5_000.0 + 1_600.0);
    // More frames than the table: the first-frame time and the frame rate.
    let times = frame_times_us(&t, 9, 25.0);
    assert_eq!((times[0], times[1]), (5_081_600.0, 5_121_600.0));
    // Lens 1 uses its own table if the file has one, else the times of lens 0.
    let [lens0, lens1] = lens_frame_times_us(&t, 8, 25.0);
    assert_eq!(lens0, lens1);
    let other = exposure_table(10, 100, |_| 0.01);
    let t = parse_trailer(&trailer(&[
        record(0x400, &table),
        record(0xc00, &other),
        fields(),
    ]))
    .unwrap();
    let [lens0, lens1] = lens_frame_times_us(&t, 8, 25.0);
    assert_eq!(lens1[0], lens0[0] + 100.0);
    assert_eq!(lens1[1], 5_120_100.0 - 5_000.0 + 1_600.0);
    // A table with a time that does not increase, or a duration that is not valid, is not used.
    let mut repeated = table.clone();
    repeated[16..24].copy_from_slice(&1u64.to_le_bytes());
    for bad in [
        exposure_table(10, 0, |i| if i == 4 { f64::NAN } else { 0.01 }),
        exposure_table(10, 0, |i| if i == 4 { -0.01 } else { 0.01 }),
        repeated,
    ] {
        let t = parse_trailer(&trailer(&[record(0x400, &bad), fields()])).unwrap();
        assert!(t.exposures.is_none());
    }
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
    let imu = turning_imu(0.001, |_| 15.0);
    for mode in [Stabilization::Lock, Stabilization::Follow] {
        assert!(matches!(
            frame_orientations(&t, &imu, &frame_times(), 24.0, mode),
            Err(Error::Unsupported(_))
        ));
    }
    assert!(frame_orientations(&t, &imu, &frame_times(), 24.0, Stabilization::Off).is_ok());
}

#[test]
fn the_full_imu_reader_removes_the_still_bias_at_the_sensor_rate() {
    // A tripod at 1 kHz with a gyroscope z bias of 8 raw units (0.49 degree per second). As Insta360, the
    // statistic gets the first 2000 samples first, thus the bias goes from the second sample. The first group
    // of 4 keeps the first sample: 8 / 4 = 2 units.
    let mut samples = imu_samples(4000, 1000, [0, 0, -1024], 1_000_000);
    for i in 0..4000 {
        samples[i * 20 + 18..i * 20 + 20].copy_from_slice(&32776u16.to_le_bytes());
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
    assert!(full.gyro_bias_removed);
    let z: Vec<u16> = full.raw.chunks(6).map(|s| s[5]).collect();
    assert_eq!(z[0], 32770);
    assert!(z[1..].iter().all(|&v| v == 32768), "{:?}", &z[..8]);

    // A sample at the end of the gyroscope range is not usable: the statistic does not get it. Else these
    // samples would make the camera look not still.
    for i in (50..4000).step_by(100) {
        samples[i * 20 + 14..i * 20 + 16].copy_from_slice(&0u16.to_le_bytes());
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
    // An unusable sample keeps its value, thus its group of 4 has 8 / 4 = 2 units.
    let z: Vec<u16> = full.raw.chunks(6).map(|s| s[5]).collect();
    for (g, &v) in z.iter().enumerate().skip(1) {
        let mixed = (g * 4..g * 4 + 4).any(|i| i % 100 == 50);
        assert_eq!(v, if mixed { 32770 } else { 32768 }, "group {g}");
    }
}

#[test]
fn the_full_imu_reader_averages_without_a_rounding_bias() {
    // 1 kHz, thus groups of 4. In each group, the gyroscope z value is 0, 1, 1, 1 (mean 0.75) and the gyroscope x
    // value is 0, 0, 0, 1 (mean 0.25). Each channel has its own remainder. The camera turns at 6 degrees per
    // second about y, thus it is not still, and the reader does not remove a bias.
    let mut samples = imu_samples(4000, 1000, [0, 0, -1024], 1_000_000);
    for i in 0..4000 {
        samples[i * 20 + 16..i * 20 + 18].copy_from_slice(&32868u16.to_le_bytes());
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

#[test]
fn rolling_rotations_span_the_sweep_around_the_frame_time() {
    let t = trailer_for(20.0);
    // 15 degrees per second: a sweep of 100 ms turns the camera by 1.5 degrees.
    let imu = turning_imu(20.0, |_| 15.0);
    let times = frame_times();
    let lens1: Vec<f64> = times.iter().map(|t| t + 2_000.0).collect();
    for mode in [Stabilization::Lock, Stabilization::Off] {
        let frames = frame_orientations(&t, &imu, &times, 24.0, mode).unwrap();
        let rows = rolling_rotations(
            &t,
            &imu,
            &frames,
            &[times.clone(), lens1.clone()],
            100_000.0,
            5,
        )
        .unwrap();
        assert_eq!(rows.len(), frames.len());
        for (frame, [lens0, lens1]) in frames.iter().zip(&rows).step_by(20) {
            assert_eq!((lens0.len(), lens1.len()), (5, 5));
            // The middle row is at the frame time: the frame rotation itself for lens 0.
            assert!(angle_between(&lens0[2], frame) < 1e-4, "{mode:?}");
            // The first and the last row: the turn of the camera in the sweep, 1.5 degrees.
            let sweep = angle_between(&lens0[4], &lens0[0]);
            assert!((sweep - 1.5).abs() < 0.05, "{mode:?}: {sweep:.3} degrees");
            // Lens 1 is 2 ms later: 0.03 degrees.
            let later = angle_between(&lens1[2], &lens0[2]);
            assert!((later - 0.03).abs() < 0.01, "{mode:?}: {later:.4} degrees");
        }
    }
    // The last row is later than the first: it is the rotation of Lock 50 ms after the frame.
    let lock = frame_orientations(&t, &imu, &times, 24.0, Stabilization::Lock).unwrap();
    let after: Vec<f64> = times.iter().map(|t| t + 50_000.0).collect();
    let later = frame_orientations(&t, &imu, &after, 24.0, Stabilization::Lock).unwrap();
    let rows = rolling_rotations(
        &t,
        &imu,
        &lock,
        &[times.clone(), times.clone()],
        100_000.0,
        5,
    )
    .unwrap();
    assert!(angle_between(&rows[10][0][4], &later[10]) < 1e-4);
    // No sweep or no IMU: no rows.
    assert!(
        rolling_rotations(&t, &imu, &lock, &[times.clone(), times.clone()], 0.0, 5).unwrap()[0][0]
            .is_empty()
    );
    assert!(
        rolling_rotations(&t, &Imu::default(), &lock, &[times.clone(), times], 1e5, 5).unwrap()[0]
            [0]
        .is_empty()
    );
}
