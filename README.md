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
```

The `photo` command keeps the full resolution. It adds an XMP GPano packet, thus other viewers
open the photo as a 360 panorama.

The `video` command needs `ffmpeg` and `ffprobe` on the `PATH`. The stitch is a remap that the
tool calculates before it starts ffmpeg. Thus, the ffmpeg encoder sets the speed.

The video output is two times as wide as one lens stream, for example 7680x3840 for the 8K video of
the X5. This width keeps approximately the resolution of the lenses. To change it, use `--width`
with a multiple of 4, to a maximum of 16384. Many phones and browsers can decode H.264 only to a
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
seekable source and stitches decoded pixels. For videos, it makes remap planes that you give to
your own ffmpeg. Thus, the core stays small, and your application keeps its image pipeline and
its transcode settings.

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
   of one hour needs only some MB of reads.
2. **Calibration.** The file contains the radius, the center and the rotation of each lens, and
   the lens type.
3. **Level.** The library integrates the motion sensor data. It gets the mean gravity and makes it
   smooth. Then it uses the value at the time of the first frame. Thus, the horizon is level and
   the sky is up, at all camera angles. The library decodes only the first two minutes after the
   first frame. If the IMU data is shorter than 10 s, the library uses the first sample only.
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
also lets you show progress. The remap planes for a 7680x3840 video take less than one second.
The ffmpeg encoder uses the remaining time. In memory, the full planes use approximately 265 MB at
this width. The CLI writes them band by band to files, thus it uses less than 20 MB.

### Video orientation

The library makes a video level only at the first frame. It uses the same remap planes for all
the frames. Thus, if the camera turns or tilts during the recording, the video also turns or tilts.

Insta360 Studio is different. Its video export changes the orientation of each frame (FlowState
stabilization and horizon lock), thus a video from a moving camera is steady and level. We think
that this is the correct target for this library. Already at the first frame, the Studio export
has a different orientation, by approximately 2 to 3 degrees.

The plan:

1. Measure the orientation of each Studio frame against our frames. This shows if Studio only
   levels the horizon or also stabilizes the direction, and how it smooths the motion.
2. Give a rotation for each frame. The library already calculates the orientation of the camera
   for the full recording from the motion sensor. Now it uses only the first frame.
3. Apply the rotation in ffmpeg after the stitch, with the `v360` filter and a command for each
   frame. This samples the image a second time, thus the image becomes a little less sharp. To
   stitch and rotate in one step, the library needs different remap planes for each frame. That
   needs a GPU path, which the library does not have.

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
