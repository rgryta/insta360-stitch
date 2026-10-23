# insta360-stitch

This library stitches Insta360 dual-fisheye photos (`.insp`) and videos (`.insv`) into
equirectangular panoramas. The result is the same as the result from Insta360 Studio, but you do
not need Insta360 Studio. All the necessary data comes from the file: the factory lens
calibration of each camera, the state of the lens guard, and the motion sensor data. The library
uses the motion sensor data to make the horizon level.

There is one Rust core. You can use it in these ways:

| Package | Installation | Use |
|---|---|---|
| CLI | Release binaries, `cargo install insta360-stitch-cli` | Stitch files manually or in scripts |
| Node.js | `npm i @cradgear/insta360-stitch` | Applications, for example [Immich](https://immich.app) |
| Python | `pip install insta360-stitch` or `uv add insta360-stitch` | Scripts and photo tools |
| Rust | `cargo add insta360-stitch` | All other applications |

**Note:** The Node.js package, the Python package and the CLI release binaries are published. The Rust
crates are not on crates.io yet. Until then, use the repository as a Git dependency or build from
the source (see [Development](#development)).

The library supports the **Insta360 X5** without a lens guard and with the premium guard. It
does not support other cameras, the standard guard, diving housings or ND filters. For these, the
library gives an "unsupported" error. To add support, see [Contributing](#contributing).

We tested these X5 modes with the premium guard:

| Type | Mode | Lens image or stream | Output |
|---|---|---|---|
| Photo | 72 MP | 5952x5952 | 11904x5952 |
| Photo | 72 MP, HDR | 5952x5952 | 11904x5952 |
| Photo | 18 MP | 2944x2944 | 5888x2944 |
| Video | 8K, 24 fps | 3840x3840 | 7680x3840 |
| Video | 5.7K, 24 fps and 60 fps | 2880x2880 | 5760x2880 |
| Video | 4K, 120 fps | 1920x1920 | 3840x1920 |

The library does not read the RAW photos (`.dng`). It does not use the low-resolution video copies
(`.lrv`).

## CLI

```sh
insta360-stitch info VID_20261023_053255_00_254.insv      # Shows the trailer data as JSON
insta360-stitch photo IMG_20261023_053237_00_253.insp out.jpg
insta360-stitch preview VID_20261023_053255_00_254.insv poster.jpg   # Writes the small preview from the camera
insta360-stitch video VID_20261023_053255_00_254.insv out.mp4 --preset veryfast
# On an Intel iGPU (for example the server of Immich): hardware decode and HEVC encode
insta360-stitch video VID.insv out.mp4 --threads 6 --hwaccel vaapi --encoder hevc_vaapi \
  --encoder-arg=-vaapi_device --encoder-arg=/dev/dri/renderD128 \
  --encoder-arg=-vf --encoder-arg=hwupload,scale_vaapi=format=nv12 --encoder-arg=-filter_threads --encoder-arg=1 \
  --encoder-arg=-low_power --encoder-arg=1 --encoder-arg=-async_depth --encoder-arg=1 \
  --encoder-arg=-qp --encoder-arg=22
```

The `photo` command keeps the full resolution. It adds an XMP GPano packet, thus other viewers
open the photo as a 360 panorama.

The `video` command needs `ffmpeg` and `ffprobe` on the `PATH`. ffmpeg decodes the two lens
streams, the tool stitches each frame, and a second ffmpeg encodes the result. The three steps
run at the same time. By default, the video is stabilized as in Insta360 Studio: the horizon is
level and the view direction does not change, also when the camera turns (`--stabilization
lock`). Use `--stabilization follow` to keep the horizon level but let the view follow slow turns
of the camera, or `--stabilization off` for the orientation of the first frame in all frames.

The encoder is libx264 by default (`--crf`, `--preset`). Use `--encoder` and `--encoder-arg` for a
different encoder, for example a hardware encoder. Use `--decoder-arg` for more ffmpeg options of
the decoder, for example `-hwaccel_device`.

At 8K, the stitch needs approximately 200 MB of memory. ffmpeg needs more:

- The decoder uses 2 threads for each lens stream by default (`--decoder-threads`). More threads do
  not make an 8K lens stream faster, but they use much more memory: at 8K with VAAPI, the decoder
  needs 1.2 GB with 2 threads and 2.3 GB with the ffmpeg default.
- libx264 needs more than 4 GB at 8K. A hardware encoder needs much less.
- With Intel VAAPI, `-low_power 1` (the fixed-function encoder) is 2 times faster than the default
  encoder and needs half the memory (1.1 GB at 8K), with the same quality at the same `-qp`.
  `scale_vaapi=format=nv12` changes the format on the GPU, not on the CPU. Some GPUs do not have the
  low-power encoder. Then remove `-low_power`.

`--threads` sets the threads of the stitch. The default is one thread for each CPU core. Use fewer
threads to keep CPU time for other services. Measured on an Intel Core 3 N355 (8 cores) with the
command above: an 8K video (7680x3840) needs approximately 0.9 s for each frame, with 6 threads or
with 8 threads. The full process needs approximately 2.6 GB. A 5.7K video at `--width 1920` gives
15 frames each second and needs 0.7 GB.

The video output is two times as wide as one lens stream, for example 7680x3840 for the 8K video of
the X5. This width keeps approximately the resolution of the lenses. To change it, use `--width`
with a multiple of 4 from 16 to 16384. Many phones and browsers can decode H.264 only to a
width of 4096. For these devices, use `--width 3840`.

The MP4 file has 360 video metadata (Google Spherical Video V1 and V2). Thus, players that read
this metadata show the video as a sphere. The `moov` box is at the start of the file, thus a player
can start before the full download. If the tool cannot add the metadata, it keeps the video
without the metadata and shows a warning.

The tool writes its temporary files in a folder next to the output, not in the system temporary
folder. That folder can be in memory, and a long 8K video can be larger than the memory. Thus,
make sure that the output folder has space for approximately two times the size of the video.

## Library

The core does not decode images, encode images or run ffmpeg. It reads the trailer from a
seekable source and stitches decoded pixels. For a stabilized video, `FrameStitcher` stitches
each decoded frame with its own rotation (Rust and the CLI only for now). The core also makes
remap planes that you give to your own ffmpeg, for a video without stabilization. The Node.js and
Python packages have only the remap planes for now. Thus, the core stays small, and your
application keeps its image pipeline and its transcode settings.

### Node.js

```js
import sharp from 'sharp';
import { Insta360File } from '@cradgear/insta360-stitch';

const path = 'IMG_20261023_053237_00_253.insp';
const file = await Insta360File.open(path); // Does not stop the event loop
if (file.stitchable) {
  const { data, info } = await sharp(path).raw().toBuffer({ resolveWithObject: true });
  const stitched = await file.stitchPhoto(data, info.width, info.height, info.channels); // Does not stop the event loop
  await sharp(stitched, { raw: info }).jpeg({ quality: 95 }).toFile('out.jpg');
} else {
  console.log(file.unsupportedReason);
}
```

For a video, `await file.videoMaps(lensWidth, lensHeight)` gives five planes. The default output
width is `2 * lensWidth`. Give the planes to ffmpeg as raw inputs with `FFMPEG_FILTER`. Then use
`await addSphericalMetadata(ffmpegOutput, finalOutput)` to add the 360 video metadata. Do not use
`-movflags +faststart` in ffmpeg, because `addSphericalMetadata` also moves the `moov` box to the
start. `addSphericalMetadata` refuses a fragmented MP4 file (`-movflags frag_keyframe`). It writes a
temporary file next to the output and renames it. Thus, after an error, the output did not change.
The `video` command of the CLI shows the full sequence.

### Python

```python
from PIL import Image
from insta360_stitch import Insta360File, UnsupportedError

file = Insta360File("IMG_20261023_053237_00_253.insp")
source = Image.open("IMG_20261023_053237_00_253.insp").convert("RGB")
stitched = file.stitch_photo(source.tobytes(), *source.size)  # Releases the GIL
Image.frombytes("RGB", source.size, stitched).save("out.jpg", quality=95)
```

Each heavy operation also has an async form: `Insta360File.open_async`, `stitch_photo_async`,
`stitch_photo_band_async`, `video_maps_async` and `add_spherical_metadata_async`. These are native coroutines. They do the work
on a different thread, thus the event loop does not wait:

```python
stitched = await file.stitch_photo_async(source.tobytes(), *source.size)
```

Each async call starts its own thread and allocates its own output. Thus, `asyncio.gather` over
many photos stitches all of them at the same time and needs memory for all of them. To limit the
memory, use an `asyncio.Semaphore`. If you cancel an async call, the work continues to the end on
its thread, and the result is not used. Do not change its input buffer until that work is complete.

The sync forms release the GIL. Thus, you can also use them from many threads at the same time.

The Python package supports free-threaded Python 3.14t and later. In free-threaded Python, the
module does not turn the GIL on. Free-threaded Python 3.13t is not supported, because PyO3 does not
support it. For all other Python versions from 3.11, one `abi3` wheel is sufficient.

The stitch functions of the Python package accept `bytes`, `bytearray`, `memoryview` and C-contiguous
`uint8` arrays, for example NumPy arrays. They read the data without a copy. Do not change the data
while the function runs.

### Errors

Each API has three types of error:

- **Unsupported**: The library cannot stitch the file, because of the camera, the lens accessory
  or the trailer layout. A retry will not help. Show the preview from the camera instead.
- **I/O**: A read operation failed, or it gave fewer bytes than necessary. A retry can help.
- **Invalid argument**: The calling code gave a wrong value, for example a buffer of the wrong size
  or a band outside the photo. Correct the calling code.

| | Unsupported | I/O | Invalid argument |
|---|---|---|---|
| Node.js (message starts with) | `unsupported:` | `i/o error:` | `invalid argument:` |
| Python | `UnsupportedError` | `OSError` | `ValueError` |
| Rust | `Error::Unsupported` | `Error::Io` | `Error::InvalidArgument` |

In Python, `UnsupportedError` is also a `ValueError`. Thus, catch `UnsupportedError` before
`ValueError`.

Use only complete files. If a copy operation did not finish, the end of the file is not there.
Then the library gives "unsupported", because it cannot find the trailer.

## Platforms

| Platform | CLI | Node.js | Python |
|---|---|---|---|
| Linux x64 and arm64 (glibc) | Yes | Yes | Yes (manylinux) |
| Linux x64 and arm64 (musl) | - | - | Yes (musllinux) |
| macOS Intel and Apple silicon | Yes | Yes | Yes (universal2) |
| Windows x64 and arm64 | Yes | Yes | Yes |

The CI tests the CLI, the Node.js package and the Python package on Linux, macOS, Windows x64 and
Windows arm64. The release builds for Linux use glibc 2.17. Thus, they also run on older
distributions, for example Debian. For a different platform, build the package from the source
code. You need a Rust toolchain for this.

## Operation

1. **Trailer.** Insta360 adds a trailer with records to the end of the JPEG or MP4 file. The
   library reads the headers first. Then it reads only the records that it needs. Thus, a video
   of one hour needs only some MB of reads. A stabilized video also reads the full IMU record
   (approximately 72 MB for one hour at 1 kHz).
2. **Calibration.** The file contains the radius, the center and the rotation of each lens, and
   the lens type.
3. **Level.** The library integrates the motion sensor data. It gets the mean gravity and makes it
   smooth. Then it uses the value at the time of the first frame. Thus, the horizon is level and
   the sky is up, at all camera angles. For a photo or the first frame, the library decodes only
   the first two minutes after the first frame. If the IMU data is shorter than 10 s, the library
   uses the first sample only. A stabilized video uses the full IMU stream and the value at the
   time of each frame. If that stream is shorter than 10 s, the library uses a shorter window.
4. **Projection.** Each output pixel is a ray. The lens polynomial gives the position of each ray
   in the two lenses. At the 18 degree seam, a smooth blend joins the two lenses. The blend is
   narrower near the nadir.

We compared the constants with exports from Insta360 Studio. The mean absolute error against the
Studio output is 4.4 levels without a guard and 7.5 levels with the premium guard. An 18 MP photo
gives 8.4 levels.

All the modes in the table use the same calibration, which the camera writes for the full sensor.
For a smaller mode, the library scales it. To check this, we compared the two lenses where they
both see the scene, at the seams. In all the tested modes, the median offset between the two lens
images is 0 pixels.

### Performance

The CLI stitches a photo of 11904x5952 pixels in approximately 2.8 s. This time includes the JPEG
decode and encode, on a desktop computer with 24 threads. The rows are independent, thus the
library uses all processor cores. With one thread, the stitch takes 11 s instead of 0.8 s.

To use fewer cores, set the `RAYON_NUM_THREADS` environment variable before the process starts.
In Node.js, each stitch also uses one thread of the libuv thread pool until it is complete. The pool
has 4 threads. To make it larger, set `UV_THREADPOOL_SIZE`.

The CLI needs approximately 230 MB of memory for this photo. The decoded source uses 212 MB. The
CLI stitches 64 rows at a time and gives them directly to the JPEG encoder. Thus, it does not keep
the full output in memory. To get the same result in your application, use `stitch_photo_band` or
`stitch_photo_rows` in Rust, `stitchPhotoBand` in Node.js, or `stitch_photo_band` in Python. Each band call
also lets you show progress. For a video, the CLI stitches one frame at a time with
`FrameStitcher` (see Video orientation). The remap planes of `video_maps` are for an ffmpeg-only
pipeline without stabilization: they take less than one second for a 7680x3840 video, and they
use approximately 265 MB at this width.

### Video orientation

Each frame of a video has its own rotation. The library integrates the motion sensor for the full
recording and levels it with the gravity, as for a photo. The time of each frame is the
first-frame time of the metadata plus the spacing of the exposure table of the camera (record
0x400). With these times, the rotation of each frame agrees with the export of Insta360 Studio:
on 8K, 5.7K and 4K clips, the remaining difference changes by less than 0.8 degree over a clip,
and it is approximately 0.5 degree at the first frame. (Before, with the orientation of the first
frame for all frames, the difference became up to 72 degrees.)

The stitch of each frame uses a lens table: the lens positions for each direction in the camera
frame, calculated one time for the video (a cube map, approximately 19 MB). For each frame, the
library rotates the output direction, reads the table and samples the two lenses. Thus, a frame
needs no trigonometry for each pixel. Insta360 Studio uses the same design on the GPU. The table
agrees with the lens model within 0.09 pixel. An 8K frame needs approximately 140 ms on 24
threads.

Lock keeps the direction of the first frame. The gyroscope has a small drift, thus the direction
can turn slowly in a long video. Follow does not have this problem.

If the IMU stream is too short for stabilization, the CLI shows a warning and uses
`--stabilization off`. If the exposure table does not have all frames, the CLI shows a warning,
and the frame times come from the frame rate.

The library does not correct the rolling shutter yet. It also does not have a GPU path yet. The
same lens table can be used on a GPU.

## Development

```sh
cargo test -p insta360-stitch -p insta360-stitch-cli
cd bindings/node && npm ci && npm run build && npm test
cd bindings/python && uv run pytest
```

Some tests need real camera files. These tests read the files from the folder in
`INSTA360_FIXTURES`. If you do not set `INSTA360_FIXTURES`, the tests do not run.

## Contributing

To add support for a different camera or lens accessory, two files are necessary:

1. A photo from that camera or with that accessory.
2. The same photo, exported from Insta360 Studio.

These files let us measure the field of view and examine the stitch.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.

Insta360 is a trademark of Arashi Vision Inc. This project has no relation to Insta360, and
Insta360 does not endorse it.
