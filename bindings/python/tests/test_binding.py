import os
import struct
from datetime import datetime, timezone
from pathlib import Path

import pytest

from insta360_stitch import FFMPEG_FILTER, Insta360File, UnsupportedError, camera_utc_offset_minutes

FIXTURES = os.environ.get("INSTA360_FIXTURES")


def test_exposes_the_ffmpeg_filter_and_the_camera_clock_helper():
    assert "maskedmerge" in FFMPEG_FILTER
    utc = int(datetime(2026, 8, 21, 2, 53, 7, tzinfo=timezone.utc).timestamp())
    assert camera_utc_offset_minutes("20260821105307", utc) == 480
    assert camera_utc_offset_minutes("20260821050207", utc) is None


def test_tells_an_unreadable_file_from_an_unsupported_one():
    with pytest.raises(OSError):
        Insta360File("/nonexistent/IMG_1.insp")
    with pytest.raises(UnsupportedError, match="no Insta360 trailer"):
        Insta360File(Path(__file__))


OFFSET = "2_2645.947_2700.400_2678.170_0.717_0.092_89.898_2646.118_8071.690_2674.010_-1.108_0.204_90.231_10752_5376_1137"


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


@pytest.fixture
def synthetic_x5(tmp_path):
    """A synthetic X5 file: a stand-in image, then a trailer with calibration and IMU data."""
    sample = (32768, 32768, 32768 - 1024, 32768, 32768, 32768)
    imu = b"".join(struct.pack("<Q6H", 1_000_000 + i * 1000, *sample) for i in range(2))
    metadata = field(5, OFFSET.encode()) + field(7, 20260821105307)
    trailer = bytearray(record(0x300, imu) + record(0x101, metadata) + bytes(72))
    struct.pack_into("<I", trailer, len(trailer) - 78 + 38, len(trailer))
    trailer[-32:] = b"8db42d694ccc418790edff439fe026bf"
    path = tmp_path / "IMG_1.insp"
    path.write_bytes(b"stand-in image" + bytes(trailer))
    return path


def test_stitches_in_full_and_in_bands_from_any_buffer(synthetic_x5):
    file = Insta360File(synthetic_x5)
    assert file.stitchable and file.local_time == "20260821105307"
    pattern = bytes(i * 7 % 251 for i in range(64 * 32 * 3))
    whole = file.stitch_photo(pattern, 64, 32)
    assert len(whole) == len(pattern)
    # A bytearray and a memoryview give the same result as bytes, without a copy.
    assert file.stitch_photo(bytearray(pattern), 64, 32) == whole
    assert file.stitch_photo(memoryview(pattern), 64, 32) == whole
    with pytest.raises(UnsupportedError):
        file.stitch_photo(bytes(27), 3, 3)
    # A band gives the same rows as the full stitch. A band after the last row is an error, also a very large one.
    assert file.stitch_photo_band(pattern, 64, 32, 10, 7) == whole[10 * 64 * 3 : 17 * 64 * 3]
    # A caller error is a ValueError, but not an UnsupportedError.
    for call in [
        lambda: file.stitch_photo_band(pattern, 64, 32, 30, 7),
        lambda: file.stitch_photo_band(pattern, 64, 32, 0, 4_000_000_000),
        lambda: file.stitch_photo(bytes(10), 64, 32),
    ]:
        with pytest.raises(ValueError) as error:
            call()
        assert not isinstance(error.value, UnsupportedError)


@pytest.mark.skipif(not FIXTURES, reason="set INSTA360_FIXTURES")
def test_reads_the_preview_of_a_real_file():
    file = Insta360File(Path(FIXTURES) / "IMG_20261023_053237_00_253.insp")
    assert file.stitchable and file.offset_state == 0
    width, height, rgb = file.preview()
    assert len(rgb) == width * height * 3
