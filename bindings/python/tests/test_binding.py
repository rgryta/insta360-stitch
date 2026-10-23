import os
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


@pytest.mark.skipif(not FIXTURES, reason="set INSTA360_FIXTURES")
def test_reads_a_real_photo_and_stitches_without_the_gil():
    file = Insta360File(Path(FIXTURES) / "IMG_20261023_053237_00_253.insp")
    assert file.stitchable and file.offset_state == 0
    width, height, rgb = file.preview()
    assert len(rgb) == width * height * 3
    raw = bytes([128]) * (64 * 32 * 3)
    assert len(file.stitch_photo(raw, 64, 32)) == len(raw)
    with pytest.raises(UnsupportedError):
        file.stitch_photo(bytes(27), 3, 3)
    # A band gives the same rows as the full stitch. A band after the last row is an error.
    pattern = bytes(i * 7 % 251 for i in range(64 * 32 * 3))
    whole = file.stitch_photo(pattern, 64, 32)
    assert file.stitch_photo_band(pattern, 64, 32, 10, 7) == whole[10 * 64 * 3 : 17 * 64 * 3]
    with pytest.raises(UnsupportedError):
        file.stitch_photo_band(pattern, 64, 32, 30, 7)
