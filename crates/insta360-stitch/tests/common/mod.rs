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

/// The v6 calibration of the same X5 (ExtraMetadata field 111), with the premium lens guard.
pub const OFFSET_V6: &str = "2_2.000000_4263.350_4264.290_2702.450_2676.740_0.664_0.009_89.927_0.000000_0.000000_0.000000_0.25607905_1.25865686_0.20002288_-4.78776312_0.00000000_-0.00101437_0.00006524_0.00924580_-0.00165139_-0.00489589_0.00112694_-0.00998045_0.00543739_10752_5376_113_2.000000_4266.190_4266.150_8069.100_2673.990_-1.126_0.323_90.198_0.000380_0.000027_-0.031672_0.25595978_1.21600473_0.49966684_-5.51673508_0.00000000_-0.00276896_-0.00022798_0.00035699_-0.00054427_0.00097417_0.00044130_0.01042031_0.00056042_10752_5376_113_394240";
