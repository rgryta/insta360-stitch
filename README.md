# insta360-stitch

This library stitches Insta360 dual-fisheye photos (`.insp`) and videos (`.insv`) into
equirectangular panoramas. The result is the same as the result from Insta360 Studio, but you do
not need Insta360 Studio. All the necessary data comes from the file: the factory lens
calibration of each camera, the state of the lens guard, and the motion sensor data. The library
uses the motion sensor data to make the horizon level and, for videos, to stabilize each frame.

There is one Rust core. You can use it in these ways:

| Package | Installation | Use |
|---|---|---|
| CLI | Release binaries, `cargo install insta360-stitch-cli` | Stitch files manually or in scripts |
| Node.js | `npm i @cradgear/insta360-stitch` | Applications, for example [Immich](https://immich.app) |
| Python | `pip install insta360-stitch` or `uv add insta360-stitch` | Scripts and photo tools |
| Kotlin / Java | Gradle: `eu.gryta:insta360-stitch`, Maven: `eu.gryta:insta360-stitch-jvm` | Android, JVM and iOS (Kotlin) applications |
| Rust | `cargo add insta360-stitch` | All other applications |

## Supported cameras

The library supports the **Insta360 X5** without a lens guard and with the premium guard, in all
its photo and video modes. For all other cameras and accessories, it gives an "unsupported" error.
The [validated camera matrix](https://github.com/rgryta/insta360-stitch/blob/main/docs/cameras.md)
gives the tested modes and the measured difference to Insta360 Studio.

## Quick start

```sh
insta360-stitch info VID_20261023_053255_00_254.insv      # Shows the trailer data as JSON
insta360-stitch photo IMG_20261023_053237_00_253.insp out.jpg
insta360-stitch preview VID_20261023_053255_00_254.insv poster.jpg   # Writes the small preview from the camera
insta360-stitch video VID_20261023_053255_00_254.insv out.mp4 --preset veryfast
```

The `video` command needs `ffmpeg` and `ffprobe` on the `PATH`. By default, it stabilizes the video
as Insta360 Studio does: the horizon is level and the view direction does not change.

```js
import { Insta360File } from '@cradgear/insta360-stitch';
const file = await Insta360File.open('IMG_20261023_053237_00_253.insp');
const stitched = await file.stitchPhoto(rgb, width, height, 3); // decoded pixels in, stitched pixels out
```

```python
from insta360_stitch import Insta360File
file = Insta360File("IMG_20261023_053237_00_253.insp")
stitched = file.stitch_photo(rgb, width, height)  # decoded pixels in, stitched pixels out
```

## How it works

1. **Trailer.** Insta360 adds a trailer with records to the end of the JPEG or MP4 file. The
   library reads the headers first. Then it reads only the records that it needs. Thus, a video
   of one hour needs only some MB of reads. A stabilized video also reads the full IMU record
   (approximately 72 MB for one hour at 1 kHz).
2. **Calibration.** As Insta360 Studio (`RenderModelType::GetOffsetInUsed`), the library uses the
   v6 calibration of the file (metadata field 111) if the file has it. For each lens, v6 gives a
   unified (MEI) projection with 13 distortion terms, the center and the rotation. Before the
   projection, the library does the same two changes as Studio:
   - With the premium lens guard, Studio changes each lens to the lens-guard design (lens type
     115, `ins::OffsetConvert::convertOffset` type 0x35). It sets xi and the radial terms of that
     design, and it fits the focal length again (least squares on the angle-to-radius curve of the
     lens, from 0 to 100 degrees). The image radius becomes approximately 2.4 % larger.
   - A video shows only a part of the calibration frame (field 27: 5312 of 5376 pixels). The
     library moves the lens centers for this crop.

   Then the library scales the calibration to the size of the lens image. A file without v6 uses
   the first calibration (field 5): the radius, the center and the rotation of each lens, and a
   polynomial for the lens type.
3. **Level.** The library integrates the motion sensor data. It gets the mean gravity and makes it
   smooth. Then it uses the value at the time of the first frame. Thus, the horizon is level and
   the sky is up, at all camera angles. For a photo or the first frame, the library decodes only
   the first two minutes after the first frame. A stabilized video uses the full IMU stream and the
   value at the time of each frame. As Insta360 Studio, an IMU stream shorter than 10 s gets one
   gravity, the mean of all its samples, and one levelling rotation. Before the integration, the
   library removes the gyroscope bias that it measures when the camera is still, as Insta360 does.
   As in Studio, the motion sensor has the nominal alignment of the camera body: the rotations of
   the calibration belong to the lenses only.
4. **Projection.** Each output pixel is a ray. The lens model gives the position of each ray in
   the two lenses.
5. **Blend.** As the Studio export, a linear ramp of 6 degrees at 90 degrees from the lens axes
   joins the two lenses. Near the bottom of the camera, the ramp becomes wider, and a mask of each
   lens image removes the parts that show the camera body (from 90.5 degrees straight down). If
   the ramp meets a masked part, the image uses a ramp of 50 degrees.
6. **Colour fusion.** As Studio ("Chromatic Calibration"), the library compares the two
   lenses in a band of 4 degrees at the seam and corrects the colour of each lens near the seam,
   so that they agree. A video gets its own correction for each frame, as in Studio. Use
   `--no-color-fusion` to stitch a video without this step.
7. **Seam flow.** As the Studio photo export, the library finds the parallax between the
   two lenses at the seam: an object near the camera is at a different position in each lens.
   It makes a grey strip of 60 x 1080 pixels along the seam circle for each lens, and calculates
   the optical flow between the two strips in each direction (DIS, ported from OpenCV, with the
   Insta360 changes: masks, flow limits and weights for rows of low texture). In the band of 82 to
   98 degrees from the axis of lens 0, each lens then moves its samples toward the other lens, so
   that the two lenses agree at the centre of the seam. A video uses the same flow for each frame,
   with the mean of the flows of the frames t - 2 to t + 2 for frame t. Studio does the same with a
   neural flow for videos and for photos smaller than 7681 pixels (the 18 MP mode), which the
   library does not have; the DIS flow aligns less well on fast objects near the camera. Use `--no-seam-flow` to stitch a video without this step.

For the measured difference to Insta360 Studio, see the
[validated camera matrix](https://github.com/rgryta/insta360-stitch/blob/main/docs/cameras.md).

### Video orientation

Each frame of a video has its own rotation. The library integrates the motion sensor for the full
recording and levels it with the gravity, as for a photo (Insta360 uses the same integration for
X5 video: attitude filter 10, `BRUTE_INTEGRATE`). The time of each frame is as in Insta360
(`GyroTimeConverter`): the middle of its exposure (the exposure table gives the end of each
exposure and its duration) plus the gyro delay of the metadata (field 28, 1.6 ms on the X5). Frame
0 is the exposure that ends at the first-frame time of the metadata. Lens 1 uses its own exposure
table (record 0xc00) if the file has one. With these times, the rotation of each frame agrees with
the export of Insta360 Studio: on 8K, 5.7K and 4K clips, the remaining difference is less than
0.2 degree, except for single frames during fast turns (up to 0.5 degree).

Lock is the Studio export with "Direction Lock" off (`kStill`): the view direction stays fixed in
the world. The gyroscope has a small drift, thus the direction can turn slowly in a long video.
Follow is the Studio export with "Direction Lock" on (`kZDirectional`): the view turns with the
heading of lens 1 (the ray to the centre of its image), smoothed as in Studio: one sample for each
frame, a centred mean over 3 s and 300 passes of a 3-tap mean. Where the lens axis points within
30 degrees of the vertical, the heading of the camera up axis bridges the gap. On a 70 s 8K/30 clip,
Follow agrees with the Studio export within 0.2 degree.

### Rolling shutter

The sensor reads its rows one after the other: the time from the first to the last row is the
rolling shutter time of the metadata (field 25: 21.2 ms at 8K, 9.2 ms at 5.7K/60, 5.5 ms at
4K/120). If the camera turns, each row sees the scene from a different direction. As Insta360
Studio (`FlowstateRender2::backMapPoint`), the library gives each row of each lens its own time,
`t - sweep / 2 + sweep * v / size`, with v the row in that lens image (the top row is the first).
For each grid point, the rotation at the frame time gives the row in each lens. Then the rotation
at the time of that row gives the position in that lens. The view correction of the frame (Lock,
Follow or Off) stays the same for all its rows. On the test clips, this makes the output nearer
to the Studio export. Use `--no-rolling-shutter` to stitch without this correction.

### Frame stitch

The stitch of each frame calculates the exact lens model on a grid, with the rotation of the
frame: one ray for each 8 x 8 output pixels at 7680 x 3840 (approximately 0.375 degree), and one
ray for each pixel below 1920. Between the grid points, the library interpolates the lens positions
linearly, because they change slowly. Thus, an 8K frame needs trigonometry for only 1/64 of its
pixels. Outside the seam, only one lens is sampled. The chroma samples (4:2:0, left siting) use the
same grid. Insta360 Studio uses a similar design on the GPU: a lens lookup map and a rotation
matrix for each frame.

The grid agrees with the lens model within 0.02 pixel, and a frame is within 0.01 level (mean) of
the exact stitch of each pixel.

For a video without stabilization, the library can also make remap planes for an ffmpeg-only
pipeline (`video_maps`). They take less than one second for a 7680x3840 video, and they use
approximately 265 MB at this width.

The library does not have a GPU path yet. The same grid can be used on a GPU.

## Performance

### Photos

The CLI stitches a photo of 11904x5952 pixels in approximately 3.6 s. This time includes the JPEG
decode and encode, on a desktop computer with 24 threads. The rows are independent, thus the
library uses all processor cores. With one thread, the CLI takes 25 s. The seam flow and the colour
fusion are calculated one time for each photo (0.4 s for the seam flow, on one thread).

The CLI needs approximately 230 MB of memory for this photo. The decoded source uses 212 MB. The
CLI stitches 64 rows at a time and gives them directly to the JPEG encoder. Thus, it does not keep
the full output in memory.

### Videos

The stitch of one 8K frame (7680x3840, yuv420p):

| Computer | 1 thread | All threads |
|---|---|---|
| Desktop, 24 threads | 0.6 s | 53 ms |
| Intel Core 3 N355, 8 cores, with other services running | 1.2 s | approximately 0.45 s |

The stitch needs approximately 200 MB of memory at 8K. ffmpeg needs more (see
[Hardware encoding](https://github.com/rgryta/insta360-stitch/blob/main/docs/hardware-encoding.md)).

The full `video` command, with the commands of
[Hardware encoding](https://github.com/rgryta/insta360-stitch/blob/main/docs/hardware-encoding.md):

| Computer | Video | Speed | Peak memory |
|---|---|---|---|
| Desktop, 24 threads, RTX 4070 Ti, NVENC | 8K, 24 fps | 10 frames/s | 4.2 GB |
| Desktop, NVENC | 5.7K, 60 fps | 21 frames/s | 2.4 GB |
| Desktop, NVENC | 4K, 120 fps | 35 frames/s | 1.4 GB |
| Desktop, libx264 (the default) | 8K, 24 fps | 2.7 frames/s | 11.6 GB |
| Intel Core 3 N355, VAAPI, `--threads 6`, other services running | 8K, 24 fps | 2.2 frames/s | 2.6 GB |
| Intel Core 3 N355, VAAPI, `--width 1920` | 5.7K, 60 fps | 15 frames/s | 0.7 GB |

At 8K, the stitch is the slowest step. With libx264, the encoder is the slowest step. The seam flow
of each frame (approximately 0.1 s) runs on two threads at the same time as the stitch of an earlier
frame. On the desktop with NVENC, an 8K video took 30% more time with the seam flow than with
`--no-seam-flow` (measured one after the other), and the CLI keeps 3 more decoded frames (130 MB at 8K). The `--width 1920`
row and the peak memory of the N355 were measured with the lens table of version 0.0.1; the grid
stitch is faster.

## Documentation

- [CLI](https://github.com/rgryta/insta360-stitch/blob/main/docs/cli.md): the commands, the video
  options, the output width and the 360 metadata.
- [Hardware encoding](https://github.com/rgryta/insta360-stitch/blob/main/docs/hardware-encoding.md):
  commands for Intel VAAPI and NVIDIA NVENC, memory and threads.
- [Library](https://github.com/rgryta/insta360-stitch/blob/main/docs/library.md): the Node.js and
  Python APIs, the errors and the platforms.
- [Validated camera matrix](https://github.com/rgryta/insta360-stitch/blob/main/docs/cameras.md):
  the cameras, accessories and modes, and the measured difference to Insta360 Studio.
- [Development](https://github.com/rgryta/insta360-stitch/blob/main/docs/development.md): build,
  tests and contributing.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.

Insta360 is a trademark of Arashi Vision Inc. This project has no relation to Insta360, and
Insta360 does not endorse it.
