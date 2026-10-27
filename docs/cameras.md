# Validated camera matrix

The library stitches only the cameras, accessories and modes in this document. For all other files,
it gives an "unsupported" error. To add a camera or an accessory, see
[Contributing](development.md#contributing).

## Cameras and accessories

| Camera | Accessory (offset state) | Calibration | Status |
|---|---|---|---|
| Insta360 X5 | Premium lens guard (5) | v6, changed to the lens-guard design | Validated against Insta360 Studio |
| Insta360 X5 | No guard (0) | v6 | Validated against Insta360 Studio |
| Insta360 X5 | Standard lens guard (4) | - | Refused: not measured |
| Insta360 X5 | Diving housing, ND filters, other states | - | Refused: not measured |
| Other cameras (lens type not 113) | - | - | Refused |

A file without the v6 calibration uses the first calibration (field 5) with the polynomial of the
lens type. The library refuses the same accessories for these files.

Without a guard, the library uses the v6 calibration without a change, as Insta360 Studio does.

## Modes

The difference is the mean absolute difference against the export of Insta360 Studio (0 to 255
levels, all channels, frame 0 for a video; lower is nearer). Most of the remaining difference of a
video comes from the two video encoders. The rotation is the rotation that aligns our video with
the Studio video best, on some frames of each clip. The rotation cost compares the high-pass images
of the two videos after that rotation (lower is nearer).

| Type | Mode | Guard | Lens image or stream | Crop (field 27) | Output | Difference | Rotation | Rotation cost |
|---|---|---|---|---|---|---|---|---|
| Photo | 72 MP | Premium | 5952x5952 | None | 11904x5952 | 0.58 | - | - |
| Photo | 72 MP, HDR | Premium | 5952x5952 | None | 11904x5952 | 1.21 | - | - |
| Photo | 18 MP | Premium | 2944x2944 | 5396 of 5376 | 5888x2944 | 1.00 | - | - |
| Photo | 72 MP | None | 5952x5952 | None | 11904x5952 | 0.68 | - | - |
| Video | 8K, 24 fps, 3 s | Premium | 3840x3840 | 5312 of 5376 | 7680x3840 | 3.76 | 0.00 to 0.02 degree | 0.07 |
| Video | 8K, 30 fps, 6 s | None | 3840x3840 | 5312 of 5376 | 7680x3840 | - | 0.00 to 0.03 degree | 0.05 to 0.06 |
| Video | 8K, 30 fps, 15 s and 70 s | Premium | 3840x3840 | 5312 of 5376 | 7680x3840 | - | less than 0.15 degree (see below) | 0.05 to 0.09 |
| Video | 5.7K, 24 fps, 3 s | Premium | 2880x2880 | 5312 of 5376 | 5760x2880 | 4.23 | 0.01 to 0.03 degree | 0.08 to 0.09 |
| Video | 5.7K, 60 fps, 4 s | Premium | 2880x2880 | 5312 of 5376 | 5760x2880 | 3.83 | 0.01 to 0.02 degree | 0.08 |
| Video | 4K, 120 fps, 3 s | Premium | 1920x1920 | 5312 of 5376 | 3840x1920 | 3.26 | 0.02 to 0.18 degree | 0.06 to 0.09 |

With `--stabilization follow` (Studio "Direction Lock" on), the 70 s clip is within 0.2 degree of the
Studio export on all measured frames (level within 0.01 degree).

On the 70 s clip, the rotation is less than 0.03 degree, except 0.12 degree at the first measured
frame. On a 15 s clip with fast turns, single frames during the turns are up to 0.5 degree from
Studio. Studio removes the first and the last 89 frames of the clips of 15 s and more; the library
keeps all frames.

The library does not read the RAW photos (`.dng`). It does not use the low-resolution video copies
(`.lrv`).

## How the library got to these values

Each step ports one part of Insta360 Studio. The differences of the 72 MP photo (premium guard) and
of the 8K/24 video:

| Step | Photo | Video, frame 0 | Video, rotation cost |
|---|---|---|---|
| First calibration, empirical field of view | 7.3 | 20.0 | 0.30 |
| v6 calibration, lens-guard and crop conversions | 6.8 | 8.1 | 0.09 |
| Mean gravity for IMU streams shorter than 10 s | 5.3 | 9.2 | 0.09 |
| Nominal alignment of the motion sensor | 0.75 | 3.78 | 0.07 |
| Export blend (6 degrees, lens masks) and colour fusion | 0.72 | 3.76 | 0.07 |
| Seam flow of the photo export (DIS) | 0.58 | 3.76 | 0.07 |

- Seam flow: the difference does not show all of the effect. At the seam, the share of the 96 x 96
  tiles (at 4096 pixels width) that are more than 1 pixel from the Studio export goes from 28% to
  1% (72 MP) and from 30% to 11% (18 MP, with a foot near the camera). The flow moves sharp details
  to their Studio position. Before, the two lenses gave a soft double image there, and a soft image
  has a smaller mean difference than a sharp image with a small offset. We think that this is why
  the difference of the 18 MP photo goes from 0.96 to 1.00.
- Seam flow of a video: Studio uses a neural flow for videos; the library uses the DIS flow of the
  photo export with the mean of 5 frames. On 7 clips (4K/120, 5.7K/24 and 60, 8K/24 and 30, with and
  without the lens guard, 3 s to 114 s; 20 frames each), the share of the seam tiles that are more
  than 1 pixel from the Studio export goes from 29% to 20%. The flow is better than no flow on 128 of
  the 140 frames. The remaining difference is largest on fast objects near the camera.
- Short IMU streams: as Studio (`GyroProcessImpl::inputGyroDataBatchWithBoxGravityFilter`), a stream
  shorter than 10 s gets one gravity, the mean of all its samples.
- Motion sensor alignment: before, the library also turned the motion sensor by the calibration
  rotation of lens 0. Studio does not: the level was 0.3 to 0.6 degree different.
- Gyroscope bias: as Studio (`InertialStatistic`), the library removes the gyroscope bias of the last
  still period before it integrates the gyroscope: the mean rate of 501 samples (at the 1 kHz sensor
  rate) when the camera does not turn. None of the test clips has such a still period, thus this
  does not change them. It is for clips from a tripod.

## Calibrate button

The calibrate function of the camera changes the motion sensor calibration (field 31) and some
other metadata. Insta360 Studio does not use field 31 for the stitch or for the stabilization: the
Studio exports of a video before and after the calibration agree with the library, which does not
use field 31. The stitch calibration of the lenses must be done outside and was not tested.
