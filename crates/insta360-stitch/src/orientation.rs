//! Levelling: which way is up when the photo (or a video's first frame) was taken.
//!
//! The camera's IMU stream is integrated (gyro) and its accelerometer gravity is box-averaged
//! over a 10 s window, normalised and smoothed with a zero-phase 2nd-order Butterworth low-pass;
//! each orientation is then levelled by an incrementally accumulated minimal rotation. Buffers
//! shorter than the box fall back to the gravity of the first sample. Every constant here was
//! matched against Insta360 Studio's exports.

use crate::error::{unsupported, Result};
use crate::math::*;
use crate::trailer::Trailer;

const GRAVITY_BOX_WINDOW_S: f64 = 10.0;
const GRAVITY_LOWPASS_HZ: f64 = 0.02;
const QUAT_TIME_OFFSET_S: f64 = 0.001;

// Signed-permutation frames of the orientation chain: the X5 IMU axes, and the
// sphere <- camera / output <- sphere conventions of the render model.
const IMU_ALIGN: Mat3 = [[-1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 1.0, 0.0]];
const SPHERE_IN: Mat3 = [[-1.0, 0.0, 0.0], [0.0, 0.0, -1.0], [0.0, -1.0, 0.0]];
const SPHERE_OUT: Mat3 = [[1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, -1.0, 0.0]];

/// Second-order Butterworth low-pass (bilinear transform with prewarp).
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

/// MATLAB-style filtfilt: odd extension of 3 * (order) samples, steady-state initial conditions.
pub fn filtfilt2(f: &Biquad, x: &[f64]) -> Vec<f64> {
    let nf = 6;
    let (b, a) = (f.b, f.a);
    // lfilter_zi: solve (I - companion(a)^T) zi = b[1:] - a[1:] * b[0]
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

/// Levelled orientation quaternions (frame 0 -> body) at `times`, or `None` when the buffer is
/// shorter than the gravity box.
pub struct GravityQuats {
    pub times: Vec<f64>,
    pub quats: Vec<Quat>,
}

pub fn box_gravity_quats(ts: &[f64], acc: &[Vec3], gyr: &[Vec3]) -> Option<GravityQuats> {
    let n = ts.len();
    if n < 3 {
        return None;
    }
    let dt = (ts[n - 1] - ts[0]) / (n - 1) as f64;
    if dt.is_nan() || dt <= 0.0 || GRAVITY_BOX_WINDOW_S / dt + 3.0 > n as f64 {
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

    let h = ((GRAVITY_BOX_WINDOW_S * 0.5) / dt).floor() as usize;
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

// a zero gravity sample would otherwise stitch an all-black image without any error
fn finite(m: Mat3) -> Result<Mat3> {
    if m.iter().flatten().any(|v| !v.is_finite()) {
        return unsupported("IMU gives no usable gravity");
    }
    Ok(m)
}

/// Output direction -> camera rotation that levels the photo (or a video's first frame).
pub fn orientation(trailer: &Trailer) -> Result<Mat3> {
    let Some(imu) = &trailer.imu else {
        // without gravity there is no "up", and an unlevelled frame is not upright either
        return unsupported("no IMU record in trailer");
    };
    let (acc_range, gyro_range) = (trailer.acc_range, trailer.gyro_range);
    let offset = &trailer.offset;
    let lens0 = rotate_matrix(offset[4], offset[5], offset[6]);
    let count = imu.len();
    let value = |i: usize, k: usize| f64::from(imu.raw[i * 6 + k]) - 32768.0;
    let acc: Vec<Vec3> = (0..count)
        .map(|i| std::array::from_fn(|k| acc_range * value(i, k) / 32768.0))
        .collect();
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

    let sequence = trailer
        .first_frame_us
        .and_then(|_| box_gravity_quats(&ts, &seq_acc, &seq_gyr));
    if let (Some(GravityQuats { times, quats }), Some(first_frame_us)) =
        (sequence, trailer.first_frame_us)
    {
        let t = first_frame_us / 1e6;
        let last = times.len() - 2;
        let k = match times.iter().position(|&time| time > t) {
            None => last,
            Some(0) => 0,
            Some(i) => (i - 1).min(last),
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
        return finite(mul(&mul(&SPHERE_IN, &qmatrix(&q)), &SPHERE_OUT));
    }
    let levelled = rotation_between(&apply(&mul(&lens0, &IMU_ALIGN), &acc[0]), &[0.0, 0.0, -1.0]);
    finite(mul(&mul(&SPHERE_IN, &transpose(&levelled)), &SPHERE_OUT))
}
