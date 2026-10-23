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
| Rust | `cargo add insta360-stitch` | All other applications |

**Note:** The Node.js package, the Python package and the CLI release binaries are published. The Rust
crates are not on crates.io yet. Until then, use the repository as a Git dependency or build from
the source (see [Development](https://github.com/rgryta/insta360-stitch/blob/main/docs/development.md)).

## Supported cameras

The library supports the **Insta360 X5** without a lens guard and with the premium guard. It
does not support other cameras, the standard guard, diving housings or ND filters. For these, the
library gives an "unsupported" error. To add support, see
[Contributing](https://github.com/rgryta/insta360-stitch/blob/main/docs/development.md#contributing).

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

## Documentation

- [CLI](https://github.com/rgryta/insta360-stitch/blob/main/docs/cli.md): the commands, the video
  options, the output width and the 360 metadata.
- [Hardware encoding](https://github.com/rgryta/insta360-stitch/blob/main/docs/hardware-encoding.md):
  commands for Intel VAAPI and NVIDIA NVENC, memory and threads.
- [Library](https://github.com/rgryta/insta360-stitch/blob/main/docs/library.md): the Node.js and
  Python APIs, the errors and the platforms.
- [How it works](https://github.com/rgryta/insta360-stitch/blob/main/docs/how-it-works.md): the
  trailer, the calibration, the level, the video stabilization and the frame stitch.
- [Performance](https://github.com/rgryta/insta360-stitch/blob/main/docs/performance.md): measured
  times and memory for photos and videos.
- [Development](https://github.com/rgryta/insta360-stitch/blob/main/docs/development.md): build,
  tests and contributing.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.

Insta360 is a trademark of Arashi Vision Inc. This project has no relation to Insta360, and
Insta360 does not endorse it.
