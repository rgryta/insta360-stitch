//! The default cut of a Bullet Time clip, as Insta360 Studio makes it. Studio finds the longest part of the clip in
//! which the camera turns around the stick. It keeps that part without its first 2 s and its last 1 s.

use insta360_stitch::math::{apply, transpose, Mat3, Vec3};

/// The rate of the analysis (samples per second).
const RATE: f64 = 50.0;
/// Half of the window of the plane test (samples): 4.2 s at `RATE`.
const HALF: usize = 105;
/// The smallest turn from one sample to the next (radians) for the turn test.
const STEP: f64 = 0.03;
/// The number of consecutive steps that must all turn more than `STEP` in the same direction.
const TURN: usize = 40;
/// The clean-up grows each flagged part by this number of samples at each end, and then removes it again.
const GROW: usize = 5;
/// The cut removes this time (s) from the start and from the end of the part that turns.
const CUT_START: f64 = 2.0;
const CUT_END: f64 = 1.0;
/// A turning part of this length (s) or shorter gives no cut.
const MIN_TURN: f64 = 3.0;
/// The frame rate of the Studio timeline of a Bullet Time clip. Studio slows the clip down to this rate.
const TIMELINE_FPS: f64 = 30000.0 / 1001.0;

/// The frames that Studio keeps by default for a Bullet Time clip: the first and the last frame (included), or `None`
/// to keep all frames. `levels`: the level rotation of each frame (output -> camera, as `--stabilization lock`),
/// `fps`: the frame rate of the clip, `gyro`: the times (s, from the first frame) of the first motion sample at or after
/// the first frame and of the last motion sample.
pub fn bullet_time_cut(levels: &[Mat3], fps: f64, gyro: (f64, f64)) -> Option<(usize, usize)> {
    let (a, b) = turning_part(&samples(levels, fps, gyro))?;
    let time = |k: usize| (gyro.0 + k as f64 / RATE).max(0.0);
    let (start, end) = (time(a), time(b));
    if end - start <= MIN_TURN {
        return None;
    }
    // Studio converts the times to frames of its slowed-down timeline, and then back to frames of the clip.
    let factor = (fps / TIMELINE_FPS).round().max(1.0) as usize;
    let frame = |t: f64| factor * ((t * 1000.0) * TIMELINE_FPS / 1000.0) as usize;
    let last = frame(end - CUT_END).min(levels.len().saturating_sub(1));
    let first = frame(start + CUT_START);
    (first <= last).then_some((first, last))
}

/// The direction that Studio follows, at the analysis rate: the world horizontal axis (output y) in camera
/// coordinates, with the axes in the Studio order (z, x, y). Between two frames, the direction is interpolated.
fn samples(levels: &[Mat3], fps: f64, (first, last): (f64, f64)) -> Vec<Vec3> {
    let per_frame: Vec<Vec3> = levels
        .iter()
        .map(|level| {
            let u = apply(&transpose(level), &[0.0, 1.0, 0.0]);
            [u[2], u[0], u[1]]
        })
        .collect();
    let end = per_frame.len().saturating_sub(1) as f64;
    (0..)
        .map(|k| first + k as f64 / RATE)
        .take_while(|&t| t < last)
        .map(|t| {
            let position = (t * fps).clamp(0.0, end);
            let (low, part) = (position.floor() as usize, position.fract());
            let high = (low + 1).min(per_frame.len() - 1);
            let v: Vec3 = std::array::from_fn(|i| {
                per_frame[low][i] * (1.0 - part) + per_frame[high][i] * part
            });
            let length = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
            v.map(|c| c / length)
        })
        .collect()
}

/// The first and the last sample of the longest turning part, before the cut.
fn turning_part(x: &[Vec3]) -> Option<(usize, usize)> {
    let mut flags = plane_flags(x);
    turn_flags(x, &mut flags);
    longest_part(&flags)
}

/// The plane test. For each sample, the singular values of the mean outer product of the directions near it. A
/// direction that turns in a plane gives two large values and one small value.
fn plane_flags(x: &[Vec3]) -> Vec<bool> {
    let n = x.len();
    let mut sums = vec![[0.0; 6]; n + 1];
    for (k, v) in x.iter().enumerate() {
        let p = [
            v[0] * v[0],
            v[0] * v[1],
            v[0] * v[2],
            v[1] * v[1],
            v[1] * v[2],
            v[2] * v[2],
        ];
        sums[k + 1] = std::array::from_fn(|i| sums[k][i] + p[i]);
    }
    let sum =
        |a: usize, b: usize| -> [f64; 6] { std::array::from_fn(|i| sums[b + 1][i] - sums[a][i]) };
    let width = 2 * HALF + 1;
    // As Studio: the windows at the ends differ, and the window in the middle has 2 * HALF samples but the divisor
    // has 2 * HALF + 1.
    let values: Vec<[f64; 3]> = (0..n)
        .map(|i| {
            let (m, count) = if i == 0 {
                return [0.0; 3];
            } else if i < HALF {
                (sum(0, 2 * i), 2 * i + 1)
            } else if i + HALF + 2 <= n {
                (sum(i + 1 - HALF, i + HALF), width)
            } else if i + 1 < n {
                let j = i + HALF + 1 - n;
                let (all, removed) = (
                    sum(n - 2 * HALF, n - 1),
                    sum(n - 2 * HALF - 1, n - 2 * HALF - 1 + 2 * j),
                );
                (std::array::from_fn(|k| all[k] - removed[k]), width - 2 * j)
            } else {
                (sum(n - 1, n - 1), 1)
            };
            eigenvalues(&m.map(|v| v / count as f64)).map(|v| v.abs() + 0.05)
        })
        .collect();
    let ratio = |a: f64, b: f64| 0.5 * (a / b + b / a) - 1.0;
    let (u, v): (Vec<f64>, Vec<f64>) = values
        .iter()
        .map(|r| {
            let r = r.map(|value| value + 0.05);
            (ratio(r[0], r[1]), ratio(r[1], r[2]))
        })
        .unzip();
    let (low, high) = v
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), &x| {
            (a.min(x), b.max(x))
        });
    if high - low < 0.1 || (high - low).is_nan() {
        return vec![false; n];
    }
    let bin_width = (high - low) / 25.0;
    let mut bins = [0usize; 25];
    for &value in &v {
        let bin = ((value - low) / bin_width - 1.0).floor();
        bins[if bin > 0.0 { (bin as usize).min(24) } else { 0 }] += 1;
    }
    if bins[0] as f64 > 0.9 * n as f64 {
        return vec![false; n];
    }
    // The threshold is at the first lowest bin. Move it down until more than 35 % of the samples are above it.
    let (mut limit, mut threshold_bin) = (25, 0);
    for _ in 0..20 {
        threshold_bin =
            (1..limit.max(1)).fold(0, |best, k| if bins[k] < bins[best] { k } else { best });
        if bins[threshold_bin..].iter().sum::<usize>() as f64 > 0.35 * n as f64 {
            break;
        }
        limit = threshold_bin;
    }
    let threshold = threshold_bin as f64 * bin_width + low;
    u.iter()
        .zip(&v)
        .map(|(&u, &v)| v > threshold && u < v)
        .collect()
}

/// The turn test. It flags the centre of each `TURN` steps in which the direction turns about the vertical axis by
/// more than `STEP` at each step, all in the same direction.
fn turn_flags(x: &[Vec3], flags: &mut [bool]) {
    let mut angle: Vec<f64> = Vec::with_capacity(x.len());
    for v in x {
        let a = v[1].atan2(v[0]);
        angle.push(match angle.last() {
            Some(&previous) => {
                a - std::f64::consts::TAU * ((a - previous) / std::f64::consts::TAU).round()
            }
            None => a,
        });
    }
    let (mut up, mut down) = (vec![0usize; x.len()], vec![0usize; x.len()]);
    for k in 1..x.len() {
        let step = angle[k] - angle[k - 1];
        up[k] = up[k - 1] + usize::from(step > STEP);
        down[k] = down[k - 1] + usize::from(step < -STEP);
    }
    for k in TURN..x.len() {
        if up[k] - up[k - TURN] >= TURN || down[k] - down[k - TURN] >= TURN {
            flags[k - TURN.div_ceil(2)] = true;
        }
    }
}

/// The longest flagged part after the clean-up, as the indices of its first and last sample. Only a part that ends
/// before the last sample counts, as in Studio.
fn longest_part(flags: &[bool]) -> Option<(usize, usize)> {
    let n = flags.len();
    let mut grown = flags.to_vec();
    for k in GROW..n.saturating_sub(GROW) {
        grown[k] |= flags[k - GROW..=k + GROW].iter().any(|&f| f);
    }
    let (mut best, mut start): (Option<(usize, usize)>, Option<usize>) = (None, None);
    for (k, &flag) in grown.iter().enumerate() {
        match (flag, start) {
            (true, None) => start = Some(k),
            (false, Some(s)) => {
                if best.is_none_or(|(a, b)| k - s > b - a + 1) {
                    best = Some((s, k - 1));
                }
                start = None;
            }
            _ => {}
        }
    }
    let (a, b) = best?;
    Some(if b - a + 1 > 2 * GROW {
        (a + GROW, b - GROW)
    } else {
        (a, b)
    })
}

/// The eigenvalues of the symmetric matrix [xx, xy, xz, yy, yz, zz], from the largest to the smallest.
fn eigenvalues(m: &[f64; 6]) -> [f64; 3] {
    let [xx, xy, xz, yy, yz, zz] = *m;
    let off = xy * xy + xz * xz + yz * yz;
    let mut values = if off < 1e-30 {
        [xx, yy, zz]
    } else {
        let q = (xx + yy + zz) / 3.0;
        let p = (((xx - q).powi(2) + (yy - q).powi(2) + (zz - q).powi(2) + 2.0 * off) / 6.0).sqrt();
        let b: [f64; 6] = [
            (xx - q) / p,
            xy / p,
            xz / p,
            (yy - q) / p,
            yz / p,
            (zz - q) / p,
        ];
        let det = b[0] * (b[3] * b[5] - b[4] * b[4]) - b[1] * (b[1] * b[5] - b[4] * b[2])
            + b[2] * (b[1] * b[4] - b[3] * b[2]);
        let phi = (det / 2.0).clamp(-1.0, 1.0).acos() / 3.0;
        let large = q + 2.0 * p * phi.cos();
        let small = q + 2.0 * p * (phi + 2.0 * std::f64::consts::PI / 3.0).cos();
        [large, 3.0 * q - large - small, small]
    };
    values.sort_by(|a, b| b.total_cmp(a));
    values
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Levels of a camera that turns at `rate` (rad/s) from `start` to `stop` (s) and is still at the other times.
    fn spin(fps: f64, seconds: f64, rate: f64, start: f64, stop: f64) -> Vec<Mat3> {
        (0..(seconds * fps) as usize)
            .map(|k| {
                let t = k as f64 / fps;
                let a = rate * (t.clamp(start, stop) - start);
                // Row 1 is the followed direction: (sin, 0, cos) -> (cos, sin, 0) in the Studio order.
                [
                    [a.cos(), 0.0, -a.sin()],
                    [a.sin(), 0.0, a.cos()],
                    [0.0, -1.0, 0.0],
                ]
            })
            .collect()
    }

    #[test]
    fn a_long_turn_keeps_the_middle_without_2_s_at_the_start_and_1_s_at_the_end() {
        let fps = 240000.0 / 1001.0;
        let levels = spin(fps, 10.0, 2.5, 0.0, 10.0);
        let (first, last) = bullet_time_cut(&levels, fps, (0.0, 10.0)).unwrap();
        // The turn test flags from 0.4 s to 9.6 s; the cut is then about 2.4 s to 8.6 s, in steps of 8 frames.
        assert_eq!((first % 8, last % 8), (0, 0));
        assert!((first as f64 / fps - 2.4).abs() < 0.1, "{first}");
        assert!((last as f64 / fps - 8.6).abs() < 0.1, "{last}");
    }

    #[test]
    fn a_short_turn_or_a_still_camera_gives_no_cut() {
        let fps = 120000.0 / 1001.0;
        assert_eq!(
            bullet_time_cut(&spin(fps, 8.0, 2.5, 3.0, 4.0), fps, (0.0, 8.0)),
            None
        );
        assert_eq!(
            bullet_time_cut(&spin(fps, 8.0, 0.0, 0.0, 8.0), fps, (0.0, 8.0)),
            None
        );
    }

    #[test]
    fn the_eigenvalues_are_those_of_the_matrix() {
        // diag(3, 2, 1) turned about z by 30 degrees.
        let (c, s) = (30f64.to_radians().cos(), 30f64.to_radians().sin());
        let m = [
            3.0 * c * c + 2.0 * s * s,
            (3.0 - 2.0) * c * s,
            0.0,
            3.0 * s * s + 2.0 * c * c,
            0.0,
            1.0,
        ];
        let e = eigenvalues(&m);
        assert!(
            (e[0] - 3.0).abs() < 1e-12 && (e[1] - 2.0).abs() < 1e-12 && (e[2] - 1.0).abs() < 1e-12,
            "{e:?}"
        );
    }
}
