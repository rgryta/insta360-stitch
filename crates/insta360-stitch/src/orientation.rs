//! This module finds the "up" direction at the time of the photo, or at the time of each frame of a video.
//!
//! It integrates the gyroscope data of the IMU stream.
//! It gets the mean gravity from the accelerometer in a 10 s window.
//! Then it normalizes the gravity and smooths it with a zero-phase, 2nd-order Butterworth low-pass filter.
//! A minimal rotation, which it adds to step by step, levels each orientation.
//! For a photo, a stream shorter than the window uses the gravity of the first sample.
//! For the frames of a video, a short stream uses a shorter window (see `frame_orientations`).
//! We compared all constants with exports from Insta360 Studio.

use crate::error::{unsupported, Result};
use crate::math::*;
use crate::trailer::{Imu, Trailer};

const GRAVITY_BOX_WINDOW_S: f64 = 10.0;
const GRAVITY_LOWPASS_HZ: f64 = 0.02;
const QUAT_TIME_OFFSET_S: f64 = 0.001;

// These are signed-permutation frames. They give the axes of the X5 IMU,
// and the sphere <- camera and output <- sphere conventions of the projection.
const IMU_ALIGN: Mat3 = [[-1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 1.0, 0.0]];
const SPHERE_IN: Mat3 = [[-1.0, 0.0, 0.0], [0.0, 0.0, -1.0], [0.0, -1.0, 0.0]];
const SPHERE_OUT: Mat3 = [[1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, -1.0, 0.0]];

/// A second-order Butterworth low-pass filter (bilinear transform with prewarp).
#[derive(Debug, Clone, Copy)]
pub struct Biquad {
    pub b: [f64; 3],
    pub a: [f64; 3],
}

pub fn butter2_lowpass(fc: f64, fs: f64) -> Biquad {
    let k = (std::f64::consts::PI * fc / fs).tan();
    let n = 1.0 / (1.0 + std::f64::consts::SQRT_2 * k + k * k);
    let b0 = k * k * n;
    Biquad {
        b: [b0, 2.0 * b0, b0],
        a: [
            1.0,
            2.0 * (k * k - 1.0) * n,
            (1.0 - std::f64::consts::SQRT_2 * k + k * k) * n,
        ],
    }
}

fn lfilter2(f: &Biquad, x: &[f64], mut z0: f64, mut z1: f64) -> Vec<f64> {
    let mut y = vec![0.0; x.len()];
    for i in 0..x.len() {
        y[i] = f.b[0] * x[i] + z0;
        z0 = f.b[1] * x[i] - f.a[1] * y[i] + z1;
        z1 = f.b[2] * x[i] - f.a[2] * y[i];
    }
    y
}

/// A zero-phase filter, the same as MATLAB filtfilt.
/// It extends the signal by 3 * (order) samples with odd symmetry and starts in the steady state.
/// A signal with 6 samples or fewer is too short for the extension. The function gives it back without a change.
pub fn filtfilt2(f: &Biquad, x: &[f64]) -> Vec<f64> {
    let nf = 6;
    if x.len() <= nf {
        return x.to_vec();
    }
    let (b, a) = (f.b, f.a);
    // This is lfilter_zi. Solve (I - companion(a)^T) zi = b[1:] - a[1:] * b[0].
    let (m00, m01, m10, m11) = (1.0 + a[1], -1.0, a[2], 1.0);
    let (r0, r1) = (b[1] - a[1] * b[0], b[2] - a[2] * b[0]);
    let det = m00 * m11 - m01 * m10;
    let zi = [(r0 * m11 - m01 * r1) / det, (m00 * r1 - m10 * r0) / det];
    let n = x.len();
    let mut ext = Vec::with_capacity(n + 2 * nf);
    ext.extend((0..nf).map(|i| 2.0 * x[0] - x[nf - i]));
    ext.extend_from_slice(x);
    ext.extend((0..nf).map(|i| 2.0 * x[n - 1] - x[n - 2 - i]));
    let mut forward = lfilter2(f, &ext, zi[0] * ext[0], zi[1] * ext[0]);
    forward.reverse();
    let mut backward = lfilter2(f, &forward, zi[0] * forward[0], zi[1] * forward[0]);
    backward.reverse();
    backward[nf..backward.len() - nf].to_vec()
}

/// Level orientation quaternions (frame 0 -> body) at `times`.
/// The function gives `None` if the stream is shorter than the gravity window.
pub struct GravityQuats {
    pub times: Vec<f64>,
    pub quats: Vec<Quat>,
}

pub fn box_gravity_quats(ts: &[f64], acc: &[Vec3], gyr: &[Vec3]) -> Option<GravityQuats> {
    box_gravity_quats_window(ts, acc, gyr, GRAVITY_BOX_WINDOW_S)
}

/// The same as [`box_gravity_quats`], with a gravity window of `window_s` seconds.
fn box_gravity_quats_window(
    ts: &[f64],
    acc: &[Vec3],
    gyr: &[Vec3],
    window_s: f64,
) -> Option<GravityQuats> {
    let n = ts.len();
    // The low-pass filter needs more than 6 samples.
    if n < 7 {
        return None;
    }
    let dt = (ts[n - 1] - ts[0]) / (n - 1) as f64;
    if dt.is_nan() || dt <= 0.0 || window_s / dt + 3.0 > n as f64 {
        return None;
    }
    let mut q: Quat = [0.0, 0.0, 0.0, 1.0];
    let mut quats = Vec::with_capacity(n);
    let mut times = Vec::with_capacity(n);
    let mut sums: Vec<Vec3> = Vec::with_capacity(n);
    quats.push(q);
    times.push(ts[0]);
    sums.push(acc[0]);
    for j in 1..n {
        let step = ts[j] - ts[j - 1];
        let th: Vec3 = std::array::from_fn(|k| -(gyr[j - 1][k] + gyr[j][k]) * 0.5 * step);
        let angle = norm(&th);
        let s = if angle <= 1e-7 {
            0.5
        } else {
            (angle * 0.5).sin() / angle
        };
        let w = if angle <= 1e-7 {
            1.0 - angle * angle / 8.0
        } else {
            (angle * 0.5).cos()
        };
        let dq: Quat = [th[0] * s, th[1] * s, th[2] * s, w];
        q = qnormalize(&qmul(&qnormalize(&dq), &q));
        quats.push(q);
        times.push((ts[j - 1] + ts[j]) * 0.5 + QUAT_TIME_OFFSET_S);
        let rotated = qrotate(&qconj(&q), &acc[j]);
        let last = sums[j - 1];
        sums.push([
            last[0] + rotated[0],
            last[1] + rotated[1],
            last[2] + rotated[2],
        ]);
    }

    let h = ((window_s * 0.5) / dt).floor() as usize;
    let mut gravity: Vec<Vec3> = vec![[0.0; 3]; n];
    for c in h..n - h {
        gravity[c] = normalize(&std::array::from_fn(|k| sums[c + h][k] - sums[c - h][k]));
    }
    let (first, last) = (gravity[h], gravity[n - h - 1]);
    gravity[..h].fill(first);
    gravity[n - h..].fill(last);
    let filter = butter2_lowpass(GRAVITY_LOWPASS_HZ, 1.0 / dt);
    let smooth: [Vec<f64>; 3] = std::array::from_fn(|k| {
        filtfilt2(&filter, &gravity.iter().map(|g| g[k]).collect::<Vec<_>>())
    });
    // A window with zero gravity gives NaN. The levelling would then turn the image 180 degrees without an error.
    // The caller then uses the first sample, which `finite` checks.
    if smooth.iter().flatten().any(|v| !v.is_finite()) {
        return None;
    }

    let mut levelling: Quat = [0.0, 0.0, 0.0, 1.0];
    let quats = quats
        .iter()
        .enumerate()
        .map(|(i, qn)| {
            let p = normalize(&qrotate(
                &levelling,
                &[-smooth[0][i], -smooth[1][i], -smooth[2][i]],
            ));
            let w = ((p[2] + 1.0) * 0.5).sqrt();
            let step: Quat = if w > 1e-6 {
                [(p[1] * 0.5) / w, (-p[0] * 0.5) / w, 0.0, w]
            } else {
                [1.0, 0.0, 0.0, 0.0]
            };
            levelling = qnormalize(&qmul(&step, &levelling));
            qmul(qn, &qconj(&levelling))
        })
        .collect();
    Some(GravityQuats { times, quats })
}

// A sample with zero gravity can stitch a black image without an error. This check prevents that.
fn finite(m: Mat3) -> Result<Mat3> {
    if m.iter().flatten().any(|v| !v.is_finite()) {
        return unsupported("IMU gives no usable gravity");
    }
    Ok(m)
}

/// The levelled orientation of the camera over the IMU stream, or the levelling of the first sample if the
/// stream is too short for the gravity window.
struct Levelled {
    sequence: Option<GravityQuats>,
    /// Not checked: `finite` checks it only when a frame uses it.
    first_sample: Mat3,
}

/// Integrates and levels the IMU stream. `with_sequence` is false when only the first sample is necessary.
/// With `fit_window`, a stream shorter than the gravity window uses a window of 80 % of its length.
/// Thus, a short video clip also gets an orientation for each frame. A photo uses the fixed window.
fn levelled(
    trailer: &Trailer,
    imu: &Imu,
    with_sequence: bool,
    fit_window: bool,
) -> Result<Levelled> {
    if imu.is_empty() {
        return unsupported("no IMU record in trailer");
    }
    let (acc_range, gyro_range) = (trailer.acc_range, trailer.gyro_range);
    let offset = &trailer.offset;
    let lens0 = rotate_matrix(offset[4], offset[5], offset[6]);
    let count = imu.len();
    let value = |i: usize, k: usize| f64::from(imu.raw[i * 6 + k]) - 32768.0;
    let acc: Vec<Vec3> = (0..count)
        .map(|i| std::array::from_fn(|k| acc_range * value(i, k) / 32768.0))
        .collect();
    let first_sample = {
        let levelled =
            rotation_between(&apply(&mul(&lens0, &IMU_ALIGN), &acc[0]), &[0.0, 0.0, -1.0]);
        mul(&mul(&SPHERE_IN, &transpose(&levelled)), &SPHERE_OUT)
    };
    if !with_sequence {
        return Ok(Levelled {
            sequence: None,
            first_sample,
        });
    }
    let gyr: Vec<Vec3> = (0..count)
        .map(|i| std::array::from_fn(|k| (gyro_range * value(i, k + 3) / 32768.0).to_radians()))
        .collect();

    let sequence_frame = mul(&IMU_ALIGN, &transpose(&lens0));
    let gyro_limit = gyro_range.to_radians();
    let (mut ts, mut seq_acc, mut seq_gyr) = (Vec::new(), Vec::new(), Vec::new());
    let mut last_ts = -1.0f64;
    for i in 0..count {
        let keep = acc[i].iter().all(|v| v.abs() < acc_range)
            && gyr[i].iter().all(|v| v.abs() < gyro_limit)
            && imu.ts[i] > last_ts;
        last_ts = last_ts.max(imu.ts[i]);
        if !keep {
            continue;
        }
        ts.push(imu.ts[i] / 1e6);
        seq_acc.push(apply(&sequence_frame, &acc[i]));
        seq_gyr.push(apply(&sequence_frame, &gyr[i]));
    }
    let window = match (fit_window, ts.first(), ts.last()) {
        (true, Some(first), Some(last)) => GRAVITY_BOX_WINDOW_S.min(0.8 * (last - first)),
        _ => GRAVITY_BOX_WINDOW_S,
    };
    Ok(Levelled {
        sequence: box_gravity_quats_window(&ts, &seq_acc, &seq_gyr, window),
        first_sample,
    })
}

/// Samples the levelled orientation at `t_us` (IMU clock) as an output direction -> camera rotation.
/// A time before the first or after the last orientation uses the nearest orientation.
fn level_at(sequence: &GravityQuats, t_us: f64) -> Result<Mat3> {
    let GravityQuats { times, quats } = sequence;
    let t = t_us / 1e6;
    let last = times.len() - 2;
    let k = match times.partition_point(|&time| time <= t) {
        0 => 0,
        i => (i - 1).min(last),
    };
    let f = ((t - times[k]) / (times[k + 1] - times[k])).clamp(0.0, 1.0);
    let (q0, q1) = (quats[k], quats[k + 1]);
    let sign = if q0[0] * q1[0] + q0[1] * q1[1] + q0[2] * q1[2] + q0[3] * q1[3] < 0.0 {
        -1.0
    } else {
        1.0
    };
    let q = qnormalize(&std::array::from_fn(|i| {
        (1.0 - f) * q0[i] + f * sign * q1[i]
    }));
    finite(mul(&mul(&SPHERE_IN, &qmatrix(&q)), &SPHERE_OUT))
}

/// Gives the output direction -> camera rotation that levels the photo, or the first frame of a video.
pub fn orientation(trailer: &Trailer) -> Result<Mat3> {
    let Some(imu) = &trailer.imu else {
        // Without gravity, there is no "up". A frame that is not level is also not upright.
        return unsupported("no IMU record in trailer");
    };
    let levelled = levelled(trailer, imu, trailer.first_frame_us.is_some(), false)?;
    match (&levelled.sequence, trailer.first_frame_us) {
        (Some(sequence), Some(first_frame_us)) => level_at(sequence, first_frame_us),
        _ => finite(levelled.first_sample),
    }
}

/// How a video turns with the camera.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stabilization {
    /// The orientation of the first frame for all the frames. The video turns and tilts with the camera.
    Off,
    /// A level horizon and a fixed view direction in each frame, as the default export of Insta360 Studio.
    Lock,
    /// A level horizon in each frame. The view direction follows slow turns of the camera, but not the shake.
    Follow,
}

/// The cutoff of the heading filter of `Stabilization::Follow`. Turns slower than approximately 3 s pass.
const FOLLOW_CUTOFF_HZ: f64 = 0.3;

/// Gives the output direction -> camera rotation for each frame time (µs, IMU clock), see [`Stabilization`].
/// Use the full IMU stream from [`crate::read_imu`] for `Lock` and `Follow`.
/// A stream shorter than the 10 s gravity window uses a shorter window. Only a stream that is too short for
/// any window (less than approximately 0.3 s) gives all frames the levelling of the first sample.
pub fn frame_orientations(
    trailer: &Trailer,
    imu: &Imu,
    times_us: &[f64],
    fps: f64,
    mode: Stabilization,
) -> Result<Vec<Mat3>> {
    if mode == Stabilization::Off {
        return Ok(vec![orientation(trailer)?; times_us.len()]);
    }
    let levelled = levelled(trailer, imu, true, true)?;
    let Some(sequence) = &levelled.sequence else {
        return Ok(vec![finite(levelled.first_sample)?; times_us.len()]);
    };
    let mut frames = times_us
        .iter()
        .map(|&t| level_at(sequence, t))
        .collect::<Result<Vec<_>>>()?;
    if mode == Stabilization::Follow && !frames.is_empty() {
        follow_heading(&mut frames, fps);
    }
    Ok(frames)
}

/// Turns each locked frame about the vertical axis by the smoothed heading change of the camera.
/// The output y axis is up. The heading is the direction of a horizontal camera axis in the output.
fn follow_heading(frames: &mut [Mat3], fps: f64) {
    // A camera axis in output coordinates: L^T e. Use the camera axis that is the most horizontal.
    let axis =
        |m: &Mat3, k: usize| apply(&transpose(m), &std::array::from_fn(|i| f64::from(i == k)));
    let tilt = |k: usize| frames.iter().map(|m| axis(m, k)[1].abs()).sum::<f64>();
    let k = (0..3).min_by(|&a, &b| tilt(a).total_cmp(&tilt(b))).unwrap();
    let mut heading: Vec<f64> = frames
        .iter()
        .map(|m| {
            let a = axis(m, k);
            a[0].atan2(a[2])
        })
        .collect();
    for i in 1..heading.len() {
        let step = (heading[i] - heading[i - 1] + std::f64::consts::PI)
            .rem_euclid(2.0 * std::f64::consts::PI)
            - std::f64::consts::PI;
        heading[i] = heading[i - 1] + step;
    }
    let smooth = if fps.is_finite() && fps > 2.0 * FOLLOW_CUTOFF_HZ {
        filtfilt2(&butter2_lowpass(FOLLOW_CUTOFF_HZ, fps), &heading)
    } else {
        heading.clone()
    };
    for (m, s) in frames.iter_mut().zip(&smooth) {
        let d = s - smooth[0];
        let (sd, cd) = d.sin_cos();
        // The heading of a camera axis in the new output is (heading - d): the slow turn of the camera stays.
        let turn: Mat3 = [[cd, 0.0, sd], [0.0, 1.0, 0.0], [-sd, 0.0, cd]];
        *m = mul(m, &turn);
    }
}
