//! Functions that make synthetic Insta360 trailers for the tests.
#![allow(dead_code)]

pub fn varint(mut value: u64) -> Vec<u8> {
    let mut bytes = Vec::new();
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            bytes.push(byte);
            return bytes;
        }
        bytes.push(byte | 0x80);
    }
}

pub fn field_varint(number: u64, value: u64) -> Vec<u8> {
    [varint(number * 8), varint(value)].concat()
}

pub fn field_f64(number: u64, value: f64) -> Vec<u8> {
    [varint(number * 8 + 1), value.to_le_bytes().to_vec()].concat()
}

pub fn field_bytes(number: u64, value: &[u8]) -> Vec<u8> {
    [
        varint(number * 8 + 2),
        varint(value.len() as u64),
        value.to_vec(),
    ]
    .concat()
}

pub fn record(id: u16, payload: &[u8]) -> Vec<u8> {
    let mut out = payload.to_vec();
    out.extend(id.to_le_bytes());
    out.extend((payload.len() as u32).to_le_bytes());
    out
}

/// An X5 calibration: two lenses, a 10752x5376 calibration frame and lens type 113.
pub const OFFSET: &str =
    "2_2645.947_2700.400_2678.170_0.717_0.092_89.898_2646.118_8071.690_2674.010_-1.108_0.204_90.231_10752_5376_1137";

/// Makes raw IMU samples at intervals of `step_us`.
/// Each sample shows the gravity `[x, y, z]` in raw units from 32768.
pub fn imu_samples(count: usize, step_us: u64, gravity: [i32; 3], start_us: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(count * 20);
    for i in 0..count as u64 {
        out.extend((start_us + i * step_us).to_le_bytes());
        for value in [gravity[0], gravity[1], gravity[2], 0, 0, 0] {
            out.extend(((32768 + value) as u16).to_le_bytes());
        }
    }
    out
}

/// Makes a trailer in the same form as the end of a real file.
/// The records come first, then the 78-byte tail with the length and the magic value.
pub fn trailer(records: &[Vec<u8>]) -> Vec<u8> {
    let mut out = records.concat();
    out.extend([0u8; 72]);
    let len = out.len();
    out[len - 78 + 38..len - 78 + 42].copy_from_slice(&(len as u32).to_le_bytes());
    out[len - 32..].copy_from_slice(b"8db42d694ccc418790edff439fe026bf");
    out
}

pub fn metadata(fields: &[Vec<u8>]) -> Vec<u8> {
    record(0x101, &fields.concat())
}
