from os import PathLike

from typing_extensions import Buffer

FFMPEG_FILTER: str
"""The ffmpeg ``-filter_complex`` for ``video_maps``.

Input 0 is the .insv file. Inputs 1 to 4 are x0, y0, x1 and y1 (``gray16le``). Input 5 is the mask (``gray``).
"""

class UnsupportedError(ValueError):
    """The library cannot stitch this file. A retry will not help."""

class Insta360File:
    """An Insta360 .insp or .insv file. The constructor reads its trailer."""

    def __init__(self, path: str | PathLike[str]) -> None:
        """Reads the trailer of the file at ``path``.

        It reads only the records that the stitcher needs, and not the image or video data.
        It raises ``UnsupportedError`` if the file has no Insta360 trailer.
        It raises ``OSError`` if the read operation fails. The constructor releases the GIL.
        """
    @staticmethod
    async def open_async(path: str | PathLike[str]) -> Insta360File:
        """The async form of the constructor. The event loop does not wait while the trailer is read."""
    @property
    def stitchable(self) -> bool:
        """Is ``True`` if the library supports the calibration, the lens guard and the IMU data."""
    @property
    def unsupported_reason(self) -> str | None:
        """The reason why the library cannot stitch the file, or ``None``."""
    @property
    def local_time(self) -> str | None:
        """The time on the camera clock when the camera made the file, as ``YYYYMMDDhhmmss``."""
    @property
    def offset_state(self) -> int:
        """The state of the lens guard or accessory: 0 = none, 5 = premium guard."""
    @property
    def is_pure_video(self) -> bool:
        """Is ``True`` for a ``PUREVIDEO`` (night) recording: the camera already filters its noise in time, and ``Denoiser`` is then too strong."""
    @property
    def preview_size(self) -> tuple[int, int] | None:
        """The width and height of the preview, or ``None`` if the trailer has no preview."""
    def preview(self) -> tuple[int, int, bytes]:
        """Gives the small preview that the camera stitched, as ``(width, height, rgb_bytes)``."""
    def stitch_photo(self, raw: Buffer, width: int, height: int, channels: int = 3) -> bytes:
        """Stitches the decoded raw pixels into an equirectangular image.

        ``raw`` can be ``bytes``, ``bytearray``, ``memoryview`` or a C-contiguous ``uint8`` array.
        The function reads it without a copy. Do not change ``raw`` while the function runs.
        The two lenses are side by side (``width == 2 * height``).
        The output has the same size and channels. This function releases the GIL.
        """
    async def stitch_photo_async(self, raw: Buffer, width: int, height: int, channels: int = 3) -> bytes:
        """The async form of ``stitch_photo``. The event loop does not wait while the photo is stitched.

        If you cancel the call, the stitch continues to the end on its thread. Do not change ``raw`` until it is complete.
        """
    def stitch_photo_band(
        self, raw: Buffer, width: int, height: int, first_row: int, rows: int, channels: int = 3
    ) -> bytes:
        """Stitches only the output rows ``first_row`` to ``first_row + rows - 1`` of the photo.

        Use it to stitch a photo in parts, for example to show progress or to limit memory.
        The arguments ``raw``, ``width``, ``height`` and ``channels`` are the same as for ``stitch_photo``.
        This function releases the GIL.
        """
    async def stitch_photo_band_async(
        self, raw: Buffer, width: int, height: int, first_row: int, rows: int, channels: int = 3
    ) -> bytes:
        """The async form of ``stitch_photo_band``."""
    def video_maps(self, lens_width: int, lens_height: int, width: int | None = None) -> dict[str, int | bytes]:
        """Makes the remap planes for a .insv file with lens streams of ``lens_width`` x ``lens_height``.

        The output is ``width`` x ``width / 2``. The default width is ``2 * lens_width``, which keeps the
        resolution of the lenses. ``width`` must be a multiple of 4 and at most 16384.
        At a width of 7680, the planes use approximately 265 MB.

        The dictionary contains ``width``, ``height``, ``x0``, ``y0``, ``x1``, ``y1`` and ``mask``.
        Give the planes to ffmpeg with ``FFMPEG_FILTER``.
        """
    async def video_maps_async(
        self, lens_width: int, lens_height: int, width: int | None = None
    ) -> dict[str, int | bytes]:
        """The async form of ``video_maps``."""

async def add_spherical_metadata_async(input: str | PathLike[str], output: str | PathLike[str]) -> None:
    """The async form of ``add_spherical_metadata``. The event loop does not wait while the file is copied."""

def add_spherical_metadata(input: str | PathLike[str], output: str | PathLike[str]) -> None:
    """Copies the MP4 file at ``input`` to ``output`` and adds the 360 video metadata (Spherical Video V1 and V2).

    It also moves the ``moov`` box to the start, the same as ffmpeg ``-movflags +faststart``.
    Give ffmpeg an output without ``+faststart``, then use this function. ``output`` must be a different file.
    The function writes a temporary file next to ``output`` and renames it. Thus, after an error, ``output`` did not change.
    It raises ``UnsupportedError`` for a fragmented MP4 file. This function releases the GIL.
    """

def camera_utc_offset_minutes(local_time: str, utc_unix_seconds: int) -> int | None:
    """Gives the UTC offset of the camera in minutes, from the camera clock and the same time in UTC.

    It gives ``None`` if the difference is not a real time zone. In that condition, the camera clock is wrong.
    """

class Denoiser:
    """The block denoise of a stitched video: the temporal filter of Insta360 Studio.

    Push the frames in order and take the filtered frames in the same order. A frame is planar YUV 4:2:0 with
    8 bit samples: ``width x height`` bytes of Y, then U, then V. Each frame is averaged with the 3 frames before and
    after it, along the motion of each block of 16 x 16 pixels. The filtered frame n is ready when the frame n + 3 has
    been pushed, and the last 3 frames come out of ``finish``. The calls must be one after the other:
    a call during another call raises ``ValueError``.
    """

    def __init__(self, width: int, height: int, strength: float = 1.0) -> None:
        """A denoise for frames of ``width`` x ``height`` pixels (even, at least 64).

        ``strength`` multiplies the knee and the limit of the filter. It raises ``ValueError`` for a wrong size or
        strength.
        """
    @property
    def frame_bytes(self) -> int:
        """The number of bytes of a frame: ``width * height * 3 // 2``."""
    def push(self, frame: Buffer) -> bytes | None:
        """Adds the next frame. Gives the filtered frame when one is ready, else ``None``. Releases the GIL."""
    async def push_async(self, frame: Buffer) -> bytes | None:
        """The async form of ``push``. The event loop does not wait while the frame is filtered.

        Do not change ``frame`` until it is complete.
        """
    def finish(self) -> bytes | None:
        """Gives the next filtered frame after the last frame has been pushed, or ``None`` when there is none.

        Call it until it gives ``None``. Releases the GIL.
        """
    async def finish_async(self) -> bytes | None:
        """The async form of ``finish``."""
