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
| `lock` (default, not for TimeShift) | Level | Fixed, as Insta360 Studio with "Direction Lock" off |
| `follow` | Level | Follows the smoothed heading of the camera, as Insta360 Studio with "Direction Lock" on |
| `off` | The orientation of the first frame for all frames | Turns with the camera |

A TimeShift recording (hyperlapse) uses `follow` by default, as Insta360 Studio. The camera keeps only some of the
frames that it captures, and the file gives the time of each one. The rotation of each frame comes from that time, and
not from the frame rate. We checked three X5 clips (a speed of 10, a speed of 60 and the automatic speed) against the
Studio export: the rotation of the frames that we sampled is within 0.5 degrees, and the Y PSNR is 32 to 35 dB (at
1920 x 960, 8 frames of each clip).

A timelapse has no IMU stream. The camera stores the attitude that it calculated for each frame, and the video uses it
with `lock`, as Insta360 Studio. We checked one X5 timelapse (5.7K lens streams, 68 frames, a still camera that was
turned by 50 degrees at the end) against the Studio export: the rotation of the 14 frames that we sampled is within
0.06 degrees, and the Y PSNR is 34 to 37 dB (at 1920 x 960, three groups of 8 frames, with `--width 8000` as the
Studio export). By default, the output keeps the resolution of the lenses (11264 x 5632).

A Bullet Time recording has lens streams with half the height (2880 x 1440 at 120 fps, 1472 x 736 at 240 fps). Each
stream shows the lower half of its lens, thus the upper half of the panorama stays black. The CLI stitches it as a 360
video. Insta360 Studio exports Bullet Time only as a flat video (16:9 and other ratios), thus we did not compare the
360 output with Studio: we checked the position of the band with the border of the lens circle in the streams, and by
eye.

`--bullet-time-view` exports a Bullet Time clip as Insta360 Studio does: a flat 16:9 video. The view looks along the
stick, and the world up is at the top of the image. The view follows the stick direction after a Gaussian smoothing
(sigma 0.11 s). The projection is stereographic with a horizontal field of view of 143 degrees. Each frame shows at
29.97 fps (slow motion), and the video has no audio. The view has no seam flow, because the stick is on the seam. We
compared two clips (120 fps and 240 fps) with the Studio export: the Y PSNR is 18.5 and 18.6 dB (a rectilinear view
without smoothing: 15.2 and 14.9 dB). The colour and the brightness of the Studio export are different, thus the
PSNR stays low. `--width` sets the width of the flat video.

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

By default, the CLI stitches the whole video, minus the camera trim (see below). `--start` and `--end`
(seconds) stitch a part of it. A frame
is in the range when its time, the frame number divided by the frame rate, is from `--start` to `--end`. The
tolerance is 1 ms. Insta360 Studio keeps the frames of a trimmed project in the same way: every source frame
in the time range, with no resampling. For example, `--start 2.96963 --end 66.7` of a 29.97 fps video gives 1911
frames, the number that Studio wrote for that trim.

Some recordings tell a time to cut at the start and at the end of the video (the camera trim, for example 3 s
at each end). Insta360 Studio cuts it by default, and the CLI does the same. The first frame is
(int)(start trim × fps / 1000). The last frame is (int)((duration − end trim) × fps / 1000), and the stitch
includes it. All times are in ms. For example, a 451-frame video at 25 fps with a trim of 3000 ms at each end
gives frames 75 to 376 (302 frames), as the Studio export. `--start` or `--end` replaces the trim at that end.
`--no-camera-trim` stitches the full video. If the trim leaves no frame, the CLI does not apply it.

A Bullet Time clip has no camera trim. Insta360 Studio cuts it to the part in which the camera turns around the
stick, and the CLI does the same. The CLI follows a horizontal direction of the world in camera coordinates, 50
times each second. It finds the longest part in which this direction turns in a plane, or turns in one direction
by more than 1.5 rad/s for 0.8 s. The cut keeps that part without its first 2 s and its last 1 s. If the part is
3 s or shorter, the CLI keeps all frames. The times change to frames on the Studio timeline of the clip (29.97 fps,
slowed down 8 times for 240 fps and 4 times for 120 fps). Thus, the first and the last frame are multiples of 8 (or
4). For example, a 240 fps clip of 1942 frames gives frames 568 to 1568 (1001 frames), as the Studio export. The
CLI writes the cut on the error output. `--start`, `--end` and `--no-camera-trim` change it as for the camera trim.

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
- We checked the filter with Studio exports of 15 clips: one still scene at automatic exposure (two clips) and at ISO
  100, 400, 1600 and 6400, four older clips at automatic exposure (8K at 24 fps, 5.7K at 24 and 60 fps, and 4K at 120
  fps), one more still scene at ISO 1600 (at 25 and at 29.97 fps), and three clips with fast camera motion (ISO 1600
  at 25 and at 29.97 fps, and ISO 6400 at 25 fps). We used 14 frames of each, encoded as Studio encodes. With the
  strengths above, the noise in flat areas is 0.93 to 1.14 times the noise of Studio. The difference to the Studio
  frames is smaller than with the earlier versions of the filter on all clips, also on the clips with fast motion. We
  looked at the parts with the fastest motion by eye and found no ghosts.
- In version 0.0.3 and earlier, the noise was 1.2 to 1.4 times the noise of Studio. The cause was the block search: in
  a flat area it found the shift whose noise was most similar to the frame, and the average of such blocks keeps the
  noise. Now each block takes the median shift of the 3 x 3 blocks around it.
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
