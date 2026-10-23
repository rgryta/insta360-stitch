"""Makes synthetic_x5.insp: a 64x32 JPEG with an X5 trailer (calibration, IMU and a 2x2 preview).

It contains no image or motion data from a camera. The calibration string is from a real X5. Run it with: uv run --no-project --with pillow python make_synthetic_x5.py
"""
import io
import pathlib
import struct

from PIL import Image

OFFSET = b"2_2645.947_2700.400_2678.170_0.717_0.092_89.898_2646.118_8071.690_2674.010_-1.108_0.204_90.231_10752_5376_1137"


def varint(value):
    out = bytearray()
    while True:
        byte, value = value & 0x7F, value >> 7
        out.append(byte | 0x80 if value else byte)
        if not value:
            return bytes(out)


def field(number, value):
    if isinstance(value, bytes):
        return varint(number * 8 + 2) + varint(len(value)) + value
    return varint(number * 8) + varint(value)


def record(record_id, payload):
    return payload + struct.pack("<HI", record_id, len(payload))


image = Image.new("RGB", (64, 32))
image.putdata([((x * 4) % 256, (y * 8) % 256, 128) for y in range(32) for x in range(64)])
jpeg = io.BytesIO()
image.save(jpeg, "JPEG", quality=90)

sample = (32768, 32768, 32768 - 1024, 32768, 32768, 32768)
imu = b"".join(struct.pack("<Q6H", 1_000_000 + i * 1000, *sample) for i in range(2))
preview = struct.pack("<6I", 1, 46, 1, 0, 2, 2).ljust(40, b"\0") + bytes([16, 235, 126, 126, 128, 128])
metadata = field(5, OFFSET) + field(7, 20260821105307)
trailer = bytearray(record(0x200, preview) + record(0x300, imu) + record(0x101, metadata) + bytes(72))
struct.pack_into("<I", trailer, len(trailer) - 78 + 38, len(trailer))
trailer[-32:] = b"8db42d694ccc418790edff439fe026bf"

out = pathlib.Path(__file__).with_name("synthetic_x5.insp")
out.write_bytes(jpeg.getvalue() + bytes(trailer))
print(out, out.stat().st_size, "bytes")
