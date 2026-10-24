# How it works

## Steps

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

## Accuracy

We compared the constants with exports from Insta360 Studio. The mean absolute error against the
Studio output is 4.4 levels without a guard and 7.5 levels with the premium guard. An 18 MP photo
gives 8.4 levels.

All the tested modes use the same calibration, which the camera writes for the full sensor. For a
smaller mode, the library scales it. To check this, we compared the two lenses where they both see
the scene, at the seams. In all the tested modes, the median offset between the two lens images is
0 pixels.

## Video orientation

Each frame of a video has its own rotation. The library integrates the motion sensor for the full
recording and levels it with the gravity, as for a photo (Insta360 uses the same integration for
X5 video: attitude filter 10, `BRUTE_INTEGRATE`). The time of each frame is as in Insta360
(`GyroTimeConverter`): the middle of its exposure (the exposure table gives the end of each
exposure and its duration) plus the gyro delay of the metadata (field 28, 1.6 ms on the X5). Frame
0 is the exposure that ends at the first-frame time of the metadata. Lens 1 uses its own exposure
table (record 0xc00) if the file has one. With these times, the rotation of each frame agrees with
the export of Insta360 Studio: on 8K, 5.7K and 4K clips, the remaining difference changes by less
than 0.7 degree over a clip, and it is approximately 0.5 to 0.7 degree at the first frame.

## Rolling shutter

The sensor reads its rows one after the other: the time from the first to the last row is the
rolling shutter time of the metadata (field 25: 21.2 ms at 8K, 9.2 ms at 5.7K/60, 5.5 ms at
4K/120). If the camera turns, each row sees the scene from a different direction. As Insta360
Studio (`FlowstateRender2::backMapPoint`), the library gives each row of each lens its own time,
`t - sweep / 2 + sweep * v / size`, with v the row in that lens image (the top row is the first).
For each grid point, the rotation at the frame time gives the row in each lens. Then the rotation
at the time of that row gives the position in that lens. The view correction of the frame (Lock,
Follow or Off) stays the same for all its rows. On the test clips, this makes the output nearer
to the Studio export (the frame-to-frame difference to Studio is 0.15 to 0.64 degree, before 0.25
to 0.79). Use `--no-rolling-shutter` to stitch without this correction.

Lock keeps the direction of the first frame. The gyroscope has a small drift, thus the direction
can turn slowly in a long video. Follow does not have this problem. Follow gets the heading from
one camera axis for the full clip: a clip in which the camera turns to point up or down is not
supported.

## Frame stitch

The stitch of each frame calculates the exact lens model on a grid, with the rotation of the
frame: one ray for each 8 x 8 output pixels at 7680 x 3840 (approximately 0.375 degree), and one
ray for each pixel below 1920. Between the grid points, the library interpolates the lens positions
linearly, because they change slowly. Thus, an 8K frame needs trigonometry for only 1/64 of its
pixels. Outside the seam, only one lens is sampled. The chroma samples (4:2:0, left siting) use the
same grid. Insta360 Studio uses a similar design on the GPU: a lens lookup map and a rotation
matrix for each frame.

The grid agrees with the lens model within 0.02 pixel (the lens table of version 0.0.1: 0.09
pixel), and a frame is within 0.01 level (mean) of the exact stitch of each pixel (before: 0.23).

For a video without stabilization, the library can also make remap planes for an ffmpeg-only
pipeline (`video_maps`). They take less than one second for a 7680x3840 video, and they use
approximately 265 MB at this width.

## Limitations

The library does not have a GPU path yet. The same grid can be used on a GPU.
