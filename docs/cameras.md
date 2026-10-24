# Validated camera matrix

The library stitches only the cameras, accessories and modes in this document. For all other files,
it gives an "unsupported" error. To add a camera or an accessory, see
[Contributing](development.md#contributing).

## Cameras and accessories

| Camera | Accessory (offset state) | Calibration | Status |
|---|---|---|---|
| Insta360 X5 | Premium lens guard (5) | v6, changed to the lens-guard design | Validated against Insta360 Studio |
| Insta360 X5 | No guard (0) | v6 | Validated against Insta360 Studio (see below) |
| Insta360 X5 | Standard lens guard (4) | - | Refused: not measured |
| Insta360 X5 | Diving housing, ND filters, other states | - | Refused: not measured |
| Other cameras (lens type not 113) | - | - | Refused |

A file without the v6 calibration uses the first calibration (field 5) with the polynomial of the
lens type. The library refuses the same accessories for these files.

Without a guard, the library uses the v6 calibration without a change, as Insta360 Studio does.
For a 72 MP photo without a guard, the difference to the Studio export is 4.1 levels (4.6 in the
seam bands). With the first calibration, it was 4.6 (5.2 in the seam bands). For an 8K/30 video of
6.4 s without a guard, the rotation cost is 0.06 to 0.08 (with the first calibration: 0.22 to 0.29).
Thus, the lens model agrees with Studio. The orientation of this clip is 1.0 to 1.3 degrees from
Studio (mostly pitch), and this difference does not change over the clip (see Short clips).

## Modes

All the modes in this table were tested with the premium lens guard. The difference is the mean
absolute difference against the export of Insta360 Studio (0 to 255 levels, all channels, frame 0
for a video). The rotation cost compares the high-pass images of the two videos after the best
rotation (lower is nearer; it is approximately 0.3 when the lens model does not agree).

| Type | Mode | Lens image or stream | Crop (field 27) | Output | Difference | Rotation cost |
|---|---|---|---|---|---|---|
| Photo | 72 MP | 5952x5952 | None | 11904x5952 | 6.8 | - |
| Photo | 72 MP, HDR | 5952x5952 | None | 11904x5952 | 9.9 | - |
| Photo | 18 MP | 2944x2944 | 5396 of 5376 | 5888x2944 | 7.8 | - |
| Video | 8K, 24 fps | 3840x3840 | 5312 of 5376 | 7680x3840 | 8.1 | 0.09 |
| Video | 8K, 30 fps (4 clips, 15 s to 114 s) | 3840x3840 | 5312 of 5376 | 7680x3840 | - | 0.07 to 0.13 |
| Video | 5.7K, 24 fps | 2880x2880 | 5312 of 5376 | 5760x2880 | 9.7 | 0.10 |
| Video | 5.7K, 60 fps | 2880x2880 | 5312 of 5376 | 5760x2880 | 9.3 | 0.10 |
| Video | 4K, 120 fps | 1920x1920 | 5312 of 5376 | 3840x1920 | 7.6 | 0.09 |

On the four 8K/30 clips, the rotation difference to Studio is less than 0.35 degree on the measured
frames (one frame during a fast turn: 0.8 degree), from the start to the end of a 114 s clip. Studio
removes the first and the last 89 frames of these clips; the library keeps all frames.

With the first calibration and an empirical field of view (the method before v6), the same files
gave 7.3, 10.7 and 8.4 for the photos, 20 to 27 for the videos and a rotation cost of 0.30 to 0.34.

The library does not read the RAW photos (`.dng`). It does not use the low-resolution video copies
(`.lrv`).

## Short clips

On clips of 3 to 7 s, the orientation of the library is 0.4 to 1.3 degrees from Studio, with the
same value for all frames of a clip. On clips of 15 s to 114 s, the difference is less than 0.35
degree. The lens model is not the cause (the rotation cost is the same as on long clips). The
library levels a clip shorter than 10 s with a gravity window of 80 % of the clip. The method of
Studio for short clips is not known yet.

## Calibrate button

The calibrate function of the camera changes the motion sensor calibration (field 31) and some
other metadata. Insta360 Studio does not use field 31 for the stitch or for the stabilization: the
Studio exports of a video before and after the calibration agree with the library, which does not
use field 31. The stitch calibration of the lenses must be done outside and was not tested.
