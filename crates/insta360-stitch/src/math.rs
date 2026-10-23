//! The 3x3 matrix and quaternion calculations that the stitcher uses.

pub type Vec3 = [f64; 3];
pub type Mat3 = [[f64; 3]; 3];
/// The order is x, y, z, w.
pub type Quat = [f64; 4];

pub const IDENTITY: Mat3 = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

pub fn mul(a: &Mat3, b: &Mat3) -> Mat3 {
    let mut out = [[0.0; 3]; 3];
    for (i, row) in a.iter().enumerate() {
        for j in 0..3 {
            out[i][j] = row[0] * b[0][j] + row[1] * b[1][j] + row[2] * b[2][j];
        }
    }
    out
}

pub fn transpose(m: &Mat3) -> Mat3 {
    [
        [m[0][0], m[1][0], m[2][0]],
        [m[0][1], m[1][1], m[2][1]],
        [m[0][2], m[1][2], m[2][2]],
    ]
}

pub fn apply(m: &Mat3, v: &Vec3) -> Vec3 {
    [
        m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
        m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
        m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
    ]
}

pub fn cross(a: &Vec3, b: &Vec3) -> Vec3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

pub fn dot(a: &Vec3, b: &Vec3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

pub fn norm(v: &Vec3) -> f64 {
    dot(v, v).sqrt()
}

pub fn normalize(v: &Vec3) -> Vec3 {
    let n = norm(v);
    [v[0] / n, v[1] / n, v[2] / n]
}

pub fn axis_rotation(axis: char, degrees: f64) -> Mat3 {
    let (s, c) = degrees.to_radians().sin_cos();
    match axis {
        'x' => [[1.0, 0.0, 0.0], [0.0, c, -s], [0.0, s, c]],
        'y' => [[c, 0.0, s], [0.0, 1.0, 0.0], [-s, 0.0, c]],
        _ => [[c, -s, 0.0], [s, c, 0.0], [0.0, 0.0, 1.0]],
    }
}

/// Gives the calibration rotation of a lens: Ry(ry + 90) * Rz(rx) * Rx(rz).
pub fn rotate_matrix(rx: f64, ry: f64, rz: f64) -> Mat3 {
    mul(
        &mul(&axis_rotation('y', ry + 90.0), &axis_rotation('z', rx)),
        &axis_rotation('x', rz),
    )
}

/// Gives the smallest rotation that turns direction a to direction b.
pub fn rotation_between(a: &Vec3, b: &Vec3) -> Mat3 {
    let (na, nb) = (normalize(a), normalize(b));
    let v = cross(&na, &nb);
    let c = dot(&na, &nb);
    let s = norm(&v);
    if s < 1e-8 {
        return if c > 0.0 {
            IDENTITY
        } else {
            [[-1.0, 0.0, 0.0], [0.0, -1.0, 0.0], [0.0, 0.0, -1.0]]
        };
    }
    let k = [[0.0, -v[2], v[1]], [v[2], 0.0, -v[0]], [-v[1], v[0], 0.0]];
    let k2 = mul(&k, &k);
    let f = (1.0 - c) / (s * s);
    let mut out = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            out[i][j] = if i == j { 1.0 } else { 0.0 } + k[i][j] + k2[i][j] * f;
        }
    }
    out
}

pub fn qmul(a: &Quat, b: &Quat) -> Quat {
    let [ax, ay, az, aw] = *a;
    let [bx, by, bz, bw] = *b;
    [
        aw * bx + ax * bw + ay * bz - az * by,
        aw * by - ax * bz + ay * bw + az * bx,
        aw * bz + ax * by - ay * bx + az * bw,
        aw * bw - ax * bx - ay * by - az * bz,
    ]
}

pub fn qconj(q: &Quat) -> Quat {
    [-q[0], -q[1], -q[2], q[3]]
}

pub fn qnormalize(q: &Quat) -> Quat {
    let n = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3]).sqrt();
    [q[0] / n, q[1] / n, q[2] / n, q[3] / n]
}

pub fn qrotate(q: &Quat, v: &Vec3) -> Vec3 {
    let u = [q[0], q[1], q[2]];
    let c = cross(&u, v);
    let t = [c[0] * 2.0, c[1] * 2.0, c[2] * 2.0];
    let ut = cross(&u, &t);
    [
        v[0] + q[3] * t[0] + ut[0],
        v[1] + q[3] * t[1] + ut[1],
        v[2] + q[3] * t[2] + ut[2],
    ]
}

pub fn qmatrix(q: &Quat) -> Mat3 {
    let [x, y, z, w] = *q;
    [
        [
            1.0 - 2.0 * (y * y + z * z),
            2.0 * (x * y - z * w),
            2.0 * (x * z + y * w),
        ],
        [
            2.0 * (x * y + z * w),
            1.0 - 2.0 * (x * x + z * z),
            2.0 * (y * z - x * w),
        ],
        [
            2.0 * (x * z - y * w),
            2.0 * (y * z + x * w),
            1.0 - 2.0 * (x * x + y * y),
        ],
    ]
}
