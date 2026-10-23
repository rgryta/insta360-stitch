# insta360-stitch

Stitch Insta360 dual-fisheye photos (`.insp`) and videos (`.insv`) into equirectangular
panoramas, the way Insta360 Studio does, without Insta360 Studio. Everything comes from the
file itself: each camera's factory lens calibration, the lens-guard state, and the motion
sensor stream used to level the horizon.

One Rust core, three ways to use it:

| | Install | For |
|---|---|---|
| CLI | release binaries, `cargo install insta360-stitch-cli` | stitching files by hand or in scripts |
| Node.js | `npm i insta360-stitch` | apps such as [Immich](https://immich.app) |
| Python | `pip install insta360-stitch` / `uv add insta360-stitch` | scripts and photo tools |
| Rust | `cargo add insta360-stitch` | everything else |

Supported today: the **Insta360 X5**, with no lens guard or with the premium guard. Other
cameras, the standard guard, diving housings and ND filters are reported as unsupported until
someone measures them (see [Contributing](#contributing)).

## CLI

```sh
insta360-stitch info VID_20261023_053255_00_254.insv      # what the trailer says, as JSON
insta360-stitch photo IMG_20261023_053237_00_253.insp out.jpg
insta360-stitch preview VID_20261023_053255_00_254.insv poster.jpg   # the camera's own small preview
insta360-stitch video VID_20261023_053255_00_254.insv out.mp4 --preset veryfast
```

Photos come out at full resolution with an XMP GPano packet, so other viewers open them as
360 panoramas. `video` needs `ffmpeg` and `ffprobe` on `PATH`; the stitch itself is a
precomputed remap, so ffmpeg's encoder sets the speed.

## Library

The core does no decoding, encoding or ffmpeg: it reads the trailer from any seekable source,
stitches decoded pixels, and builds remap planes a caller feeds to its own ffmpeg. That keeps
it small and lets an application keep its own image pipeline and transcoding settings.

### Node.js

```js
import sharp from 'sharp';
import { Insta360File } from 'insta360-stitch';

const path = 'IMG_20261023_053237_00_253.insp';
const file = Insta360File.open(path);
if (file.stitchable) {
  const { data, info } = await sharp(path).raw().toBuffer({ resolveWithObject: true });
  const stitched = await file.stitchPhoto(data, info.width, info.height, info.channels); // off the event loop
  await sharp(stitched, { raw: info }).jpeg({ quality: 95 }).toFile('out.jpg');
} else {
  console.log(file.unsupportedReason);
}
```

Videos: `await file.videoMaps(lensWidth, lensHeight)` returns five planes; pass them to ffmpeg
as raw inputs with `FFMPEG_FILTER` (see the CLI's `video` command for the exact invocation).

### Python

```python
from PIL import Image
from insta360_stitch import Insta360File, UnsupportedError

file = Insta360File("IMG_20261023_053237_00_253.insp")
source = Image.open("IMG_20261023_053237_00_253.insp").convert("RGB")
stitched = file.stitch_photo(source.tobytes(), *source.size)  # releases the GIL
Image.frombytes("RGB", source.size, stitched).save("out.jpg", quality=95)
```

### Errors

Every API separates two failures: **unsupported** (another camera, lens accessory or trailer
layout: retrying won't help, show the camera's preview instead) and **I/O** (a read failed or
came back short: worth a retry). Node errors start with `unsupported:` or `i/o error:`; Python
raises `UnsupportedError` (a `ValueError`) or `OSError`; Rust returns `Error::Unsupported` /
`Error::Io`.

## How it works

1. **Trailer.** Insta360 appends a record-based trailer to the JPEG / MP4. It is read record by
   record (headers first, then only what is needed), so a one-hour video costs a few MB of
   reads, not the whole trailer.
2. **Calibration.** Per-lens radius, centre and rotation, plus the lens type, from the file.
3. **Levelling.** The motion sensor stream is integrated and its gravity box-filtered and
   smoothed, then sampled when the first frame was exposed: the horizon comes out level and
   the sky up, whatever angle the camera was held at. Only the first two minutes past the first
   frame are decoded.
4. **Projection.** Each output pixel maps to a ray, each ray to both lenses through the lens
   polynomial; the 18 degree seam is a smooth blend, narrowed towards the nadir.

The constants were matched against Insta360 Studio's exports (mean absolute error against
Studio's own output: 4.3 levels without a guard, 7.3 with the premium guard).

### Performance

A 11904x5952 photo stitches in about 2.5 s including JPEG decode and encode (CLI, 24-thread
desktop); rows are independent and spread over all cores. Video remap planes for 3840x1920
build in well under a second; the rest is ffmpeg.

Videos are levelled once, at the first frame; per-frame stabilisation needs a GPU path and is
not implemented.

## Development

```sh
cargo test --workspace --exclude insta360-stitch-node --exclude insta360-stitch-python
cd bindings/node && npm ci && npm run build && npm test
cd bindings/python && uv run pytest
```

Tests that need real camera files read them from `INSTA360_FIXTURES` and are skipped otherwise.

## Contributing

Support for another camera or lens accessory needs one photo shot with it plus the same photo
exported from Insta360 Studio, to measure its field of view and check the stitch.

## License

[MIT](LICENSE).

Insta360 is a trademark of Arashi Vision Inc. This project is not affiliated with or endorsed
by Insta360.
