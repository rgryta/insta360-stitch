import asyncio
import os
import struct
from datetime import datetime, timezone
from pathlib import Path

import pytest

from insta360_stitch import (
    FFMPEG_FILTER,
    Insta360File,
    UnsupportedError,
    add_spherical_metadata,
    add_spherical_metadata_async,
    camera_utc_offset_minutes,
)

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


def make_box(kind, *parts):
    body = b"".join(parts)
    return struct.pack(">I", 8 + len(body)) + kind + body


def full_box(kind, *parts):
    return make_box(kind, bytes(4), *parts)


def minimal_mp4():
    """A minimal MP4 file as ffmpeg writes it without faststart: ftyp, mdat, then moov with one video track."""
    ftyp = make_box(b"ftyp", b"isom\0\0\x02\0isomiso2")
    mdat = make_box(b"mdat", b"VIDEO-ONE")
    entry = make_box(b"avc1", bytes(78), make_box(b"avcC", b"\x01\x02\x03"))
    stbl = make_box(
        b"stbl",
        full_box(b"stsd", struct.pack(">I", 1), entry),
        full_box(b"stco", struct.pack(">II", 1, len(ftyp) + 8)),
    )
    hdlr = full_box(b"hdlr", bytes(4), b"vide", bytes(13))
    trak = make_box(b"trak", full_box(b"tkhd", bytes(8)), make_box(b"mdia", hdlr, make_box(b"minf", stbl)))
    return ftyp + mdat + make_box(b"moov", full_box(b"mvhd", bytes(8)), trak)


def test_makes_video_maps_twice_as_wide_as_one_lens_by_default(synthetic_x5):
    file = Insta360File(synthetic_x5)
    maps = file.video_maps(64, 64)
    assert (maps["width"], maps["height"], len(maps["mask"]), len(maps["x0"])) == (128, 64, 128 * 64, 128 * 64 * 2)
    narrow = file.video_maps(64, 64, 64)
    assert (narrow["width"], narrow["height"]) == (64, 32)
    for width in (126, 127):
        with pytest.raises(ValueError):
            file.video_maps(64, 64, width)


def test_adds_360_video_metadata_to_an_mp4_file(tmp_path):
    source, target = tmp_path / "in.mp4", tmp_path / "out.mp4"
    source.write_bytes(minimal_mp4())
    add_spherical_metadata(source, target)
    out = target.read_bytes()
    first = struct.unpack(">I", out[:4])[0]
    assert out[4:8] == b"ftyp" and out[first + 4 : first + 8] == b"moov"
    assert b"sv3d" in out and b"st3d" in out and b"GSpherical:Spherical>true" in out
    with pytest.raises(ValueError) as error:
        add_spherical_metadata(source, source)
    assert not isinstance(error.value, UnsupportedError)
    # A bad input does not change the earlier output.
    source.write_bytes(b"not an mp4 file")
    with pytest.raises(UnsupportedError):
        add_spherical_metadata(source, target)
    assert target.read_bytes() == out


def test_async_forms_give_the_same_results_without_stopping_the_event_loop(synthetic_x5, tmp_path):
    file = Insta360File(synthetic_x5)
    pattern = bytes(i * 7 % 251 for i in range(64 * 32 * 3))

    async def main():
        whole = await file.stitch_photo_async(pattern, 64, 32)
        assert whole == file.stitch_photo(pattern, 64, 32)
        band = await file.stitch_photo_band_async(memoryview(pattern), 64, 32, 10, 7)
        assert band == whole[10 * 64 * 3 : 17 * 64 * 3]
        maps = await file.video_maps_async(64, 64)
        assert maps == file.video_maps(64, 64)

        # The errors keep their types.
        with pytest.raises(UnsupportedError):
            await file.stitch_photo_async(bytes(27), 3, 3)
        with pytest.raises(ValueError) as error:
            await file.stitch_photo_band_async(pattern, 64, 32, 0, 4_000_000_000)
        assert not isinstance(error.value, UnsupportedError)

        # The work runs on a different thread: a task on the event loop continues during a large stitch.
        large = bytes(4096 * 2048 * 3)
        ticks = 0
        done = False

        async def ticker():
            nonlocal ticks
            while not done:
                ticks += 1
                await asyncio.sleep(0)

        task = asyncio.create_task(ticker())
        await file.stitch_photo_async(large, 4096, 2048)
        done = True
        await task
        assert ticks > 10, f"the event loop ran only {ticks} times during the stitch"

        source, target = tmp_path / "in.mp4", tmp_path / "out.mp4"
        source.write_bytes(minimal_mp4())
        await add_spherical_metadata_async(source, target)
        assert b"sv3d" in target.read_bytes()

    asyncio.run(main())


@pytest.mark.skipif(not FIXTURES, reason="set INSTA360_FIXTURES")
def test_reads_the_preview_of_a_real_file():
    file = Insta360File(Path(FIXTURES) / "IMG_20261023_053237_00_253.insp")
    assert file.stitchable and file.offset_state == 0
    width, height, rgb = file.preview()
    assert len(rgb) == width * height * 3
