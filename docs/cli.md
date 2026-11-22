# CLI

```sh
insta360-stitch info VID_20261023_053255_00_254.insv      # Shows the trailer data as JSON
insta360-stitch photo IMG_20261023_053237_00_253.insp out.jpg
insta360-stitch preview VID_20261023_053255_00_254.insv poster.jpg   # Writes the small preview from the camera
insta360-stitch video VID_20261023_053255_00_254.insv out.mp4 --preset veryfast
```

Use `insta360-stitch <command> --help` for all the options of a command. `--threads` is an option
of all commands: it sets the threads of the stitch. The default is one thread for each CPU core.
Use fewer threads to keep CPU time for other services on the same computer.

## Photo

The `photo` command keeps the full resolution. It adds an XMP GPano packet, thus other viewers
open the photo as a 360 panorama. `--quality` sets the JPEG quality (1 to 100, default 95).

### HDR bracket (external HDR)

An HDR bracket (AEB) is three photos of the same scene with different exposures, taken a few seconds apart. Each
file has its own IMU data, thus the level of the horizon is a little different in each photo (0.2 to 0.4 degrees in
our test). In our test, Insta360 Studio stitched all the exposures of a bracket with the level of the first photo, so
that they line up for the merge. `--level-from` does the same: it takes the level from the IMU data of the photo that
you give.

```sh
insta360-stitch photo IMG_..._011.insp 011.jpg
insta360-stitch photo IMG_..._012.insp 012.jpg --level-from IMG_..._011.insp
insta360-stitch photo IMG_..._013.insp 013.jpg --level-from IMG_..._011.insp
```

We checked this on one bracket of three photos. Without `--level-from`, the Y PSNR against the exports of Studio was
33 dB for the first photo and 24 dB for the two others, which were 2.6 and 4.8 pixels off (at 4096 pixels of width).
With it, the two others reach 34 and 35 dB, and they are within 0.01 pixel of Studio. The first photo was also the 0 EV
photo, with the middle brightness. Thus, we do not know which of these Studio uses to select it. The command does not
merge the exposures.

## Video

The `video` command needs `ffmpeg` and `ffprobe` on the `PATH`. ffmpeg decodes the two lens
streams, the tool stitches each frame, and a second ffmpeg encodes the result. The three steps
run at the same time.

### Stabilization

By default, the video is stabilized as in Insta360 Studio: the horizon is level and the view
direction does not change, also when the camera turns (`--stabilization lock`).

| `--stabilization` | Horizon | View direction |
|---|---|---|
| `lock` (default) | Level | Fixed, as Insta360 Studio with "Direction Lock" off |
| `follow` | Level | Follows the smoothed heading of the camera, as Insta360 Studio with "Direction Lock" on |
| `off` | The orientation of the first frame for all frames | Turns with the camera |

If the IMU stream is too short for stabilization, the CLI shows a warning and uses
`--stabilization off`. If the exposure table does not have all frames, the CLI shows a warning,
and the frame times come from the frame rate. See
[Video orientation](../README.md#video-orientation).

### Encoder and decoder

The encoder is libx264 by default (`--crf`, `--preset`). Use `--encoder` and `--encoder-arg` for a
different encoder, for example a hardware encoder. `--hwaccel` sets the hardware decoder of
ffmpeg, for example `vaapi`, `qsv`, `cuda` or `d3d11va`. `--decoder-threads` sets the threads of
the decoder for each lens stream (default 2). Use `--decoder-arg` for more ffmpeg options of the
decoder, for example `-hwaccel_device`. An option of the user replaces a default of the tool.

For commands with Intel VAAPI and NVIDIA NVENC, see [Hardware encoding](hardware-encoding.md).

### Studio-like encode

`--studio-encode` makes the output like the Insta360 Studio export: HEVC (libx265, Main profile, tag `hvc1`),
full range BT.709, no B-frames, and one key frame each second (a GOP of the rounded frame rate, so 30 at
29.97 fps). The bitrate is the bitrate of the whole `.insv` file (its size divided by the duration of the
video). On four clips, the Studio export had this bitrate within 1.4%; the sum of the bitrates of the two lens
streams was up to 5% too low. `--preset` still sets the speed. `--studio-encode` does not go with `--encoder` or
`--crf`; `--encoder-arg` options come last and can change the result.

The encoder aims for the bitrate over the whole video, thus a short clip can end a few percent under it (a clip
of 4 s ended 4% under). The file is large: approximately 150 Mbit/s for an X5 clip at 8K, as the Studio export.

### Rolling shutter

By default, the CLI corrects the rolling shutter of the camera, as Insta360 Studio does: each
sensor row gets the rotation at its own time. Use `--no-rolling-shutter` to stitch without this
correction. See [Rolling shutter](../README.md#rolling-shutter).

### Seam flow

By default, the CLI aligns the two lenses at the seam with the seam flow of the photo export: each
frame gets the mean of the flows of the frames t - 2 to t + 2. Objects near the camera then do not
show two times at the seam. Use `--no-seam-flow` to stitch without it: the stitch is faster and
uses less memory. See [Seam flow](../README.md#how-it-works). With `--threads`, the seam flow and the
colour fusion use 1 or 2 of those threads (with `--threads 1`, the video uses 2 threads).

### Colour fusion

By default, the CLI matches the colours of the two lenses at the seam, as the Studio export
("Chromatic Calibration"). It calculates the correction for each frame, and it changes only the pixels
near the seam. Use `--no-color-fusion` to stitch without it. When the stitch is the slow step (a fast
encoder), an 8K video took approximately 11% more time with the colour fusion. The CLI needs a full-range
source, as the X5 records (BT.709). With another source, it stitches without the colour fusion and says so.

### Time range

By default, the CLI stitches the whole video. `--start` and `--end` (seconds) stitch a part of it. A frame
is in the range when its time, the frame number divided by the frame rate, is from `--start` to `--end`. The
tolerance is 1 ms. Insta360 Studio keeps the frames of a trimmed project in the same way: every source frame
in the time range, with no resampling. For example, `--start 2.96963 --end 66.7` of a 29.97 fps video gives 1911
frames, the number that Studio wrote for that trim.

- The decoder seeks to the start, and it stops after the last frame of the range.
- The audio gets the same range. The CLI copies it, thus ffmpeg cuts it at a packet (approximately 21 ms).
- The motion sensor data of the whole video is still read. At the start and the end of the range, the seam flow
  and the denoise have fewer frames beside a frame.

### Denoise

By default, the CLI does not filter the noise. Use `--denoise` to filter it as the Studio block denoise
does. The filter averages each frame with the 3 frames before it and the 3 frames after it. A search for each
block of 16 x 16 pixels finds where the block moved, and a pixel gets less weight when it differs more from
the pixel of the frame itself. A block that does not match gets no weight. The result stays within 4 levels of
the original pixel.

- Insta360 Studio filters the clips that the camera has not filtered in time. The CLI does not check this: it
  filters when you ask. Do not use it for a clip that the camera filters in time (the `PUREVIDEO` mode of a night
  video): Studio filters such a clip only a little, and `--denoise` is then too strong. The CLI shows a warning when
  you use `--denoise` on such a clip (`info` shows the `subMediaType` of the file: 20 is `PUREVIDEO`).
- The default strength is 1. We tuned it on five X5 clips (8K at 24 and 30 fps, 5.7K, 4K at 120 fps, and a night
  clip). Studio filters a video with a high ISO more strongly. If you set the ISO by hand (manual exposure), the
  CLI takes the strength from it and shows both numbers: 1 at ISO 100 or less, 2 at ISO 400, 3 at ISO 1600 and 4
  at ISO 6400 (and 4 above that), and the values between follow the same line against the logarithm of the ISO.
  With automatic exposure the camera does not give the ISO of the frames (`info` shows `iso` as `null`), thus the
  strength is 1. `--denoise-strength` sets the strength: a factor of the default, more than 0 and at most 16. A
  larger factor filters more noise, and it changes the pixels more.
- We checked the strengths with Studio exports of one still scene at ISO 100, 400, 1600 and 6400. With the
  strengths above, the noise in flat areas is 1.2 to 1.4 times the noise of Studio at all four ISO values. With
  strength 1 it is 1.2 times at ISO 100 and 1.5 times at ISO 6400. A strength above 4 does not filter more. The
  rest of the difference is not from the strength: Studio filters more than this filter does. We also compared
  three clips that were recorded with fast camera motion (manual ISO 1600 at 25 and at 29.97 fps, and ISO 6400 at 25
  fps; 14 frames of each). The strengths above are closer to the Studio export than strength 1 on all three clips. At
  ISO 6400, strength 4 is a little less close than strength 3 (about 2% in the luma difference), but it is still closer
  than strength 1. We compared numbers only, and we did not look for ghosting by eye. One static scene at ISO 1600 gave
  the same ratio to Studio at 25 and at 29.97 fps (1.40 and 1.43).
- It is slow at 8K and needs approximately 7 frames of memory. On 12 cores, 63 frames of 8K took 13 s without the
  filter and 27 s with it. The CLI uses the threads of `--threads`.

### Output width

The video output is two times as wide as one lens stream, for example 7680x3840 for the 8K video of
the X5. This width keeps approximately the resolution of the lenses. To change it, use `--width`
with a multiple of 4 from 16 to 16384. Many phones and browsers can decode H.264 only to a
width of 4096. For these devices, use `--width 3840`. For an output that is narrower than two lens
streams, ffmpeg scales the lenses down before the stitch.

### Output file

The MP4 file has 360 video metadata (Google Spherical Video V1 and V2). Thus, players that read
this metadata show the video as a sphere. The `moov` box is at the start of the file, thus a player
can start before the full download. If the tool cannot add the metadata, it keeps the video
without the metadata and shows a warning.

The output keeps the global metadata of the `.insv` file, for example `creation_time`, the audio
and the colour tags (the X5 records full range, BT.709).

The tool writes its temporary files in a folder next to the output, not in the system temporary
folder. That folder can be in memory, and a long 8K video can be larger than the memory. Thus,
make sure that the output folder has space for approximately two times the size of the video.

### Errors

If the ffmpeg encoder or decoder stops, the CLI stops with an error that gives its exit status.
A process that the system stopped because of too little memory gives a signal (for example
`signal: 9 (SIGKILL)`). Then use fewer decoder threads or a hardware encoder (see
[Hardware encoding](hardware-encoding.md)).
