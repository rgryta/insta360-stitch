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

## Video

The `video` command needs `ffmpeg` and `ffprobe` on the `PATH`. ffmpeg decodes the two lens
streams, the tool stitches each frame, and a second ffmpeg encodes the result. The three steps
run at the same time.

### Stabilization

By default, the video is stabilized as in Insta360 Studio: the horizon is level and the view
direction does not change, also when the camera turns (`--stabilization lock`).

| `--stabilization` | Horizon | View direction |
|---|---|---|
| `lock` (default) | Level | Fixed, as in Insta360 Studio |
| `follow` | Level | Follows slow turns of the camera, but not the shake |
| `off` | The orientation of the first frame for all frames | Turns with the camera |

If the IMU stream is too short for stabilization, the CLI shows a warning and uses
`--stabilization off`. If the exposure table does not have all frames, the CLI shows a warning,
and the frame times come from the frame rate. See
[Video orientation](how-it-works.md#video-orientation).

### Encoder and decoder

The encoder is libx264 by default (`--crf`, `--preset`). Use `--encoder` and `--encoder-arg` for a
different encoder, for example a hardware encoder. `--hwaccel` sets the hardware decoder of
ffmpeg, for example `vaapi`, `qsv`, `cuda` or `d3d11va`. `--decoder-threads` sets the threads of
the decoder for each lens stream (default 2). Use `--decoder-arg` for more ffmpeg options of the
decoder, for example `-hwaccel_device`. An option of the user replaces a default of the tool.

For commands with Intel VAAPI and NVIDIA NVENC, see [Hardware encoding](hardware-encoding.md).

### Rolling shutter

By default, the CLI corrects the rolling shutter of the camera, as Insta360 Studio does: each
sensor row gets the rotation at its own time. Use `--no-rolling-shutter` to stitch without this
correction. See [Rolling shutter](how-it-works.md#rolling-shutter).

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
