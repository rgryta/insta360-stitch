# Performance

## Photos

The CLI stitches a photo of 11904x5952 pixels in approximately 2.8 s. This time includes the JPEG
decode and encode, on a desktop computer with 24 threads. The rows are independent, thus the
library uses all processor cores. With one thread, the stitch takes 11 s instead of 0.8 s.

The CLI needs approximately 230 MB of memory for this photo. The decoded source uses 212 MB. The
CLI stitches 64 rows at a time and gives them directly to the JPEG encoder. Thus, it does not keep
the full output in memory.

## Videos

The stitch of one 8K frame (7680x3840, yuv420p):

| Computer | 1 thread | All threads |
|---|---|---|
| Desktop, 24 threads | 0.6 s | 53 ms |
| Intel Core 3 N355, 8 cores, with other services running | 1.2 s | approximately 0.45 s |

The stitch needs approximately 200 MB of memory at 8K. ffmpeg needs more (see
[Hardware encoding](hardware-encoding.md)).

The full `video` command, with the commands of [Hardware encoding](hardware-encoding.md):

| Computer | Video | Speed | Peak memory |
|---|---|---|---|
| Desktop, 24 threads, RTX 4070 Ti, NVENC | 8K, 24 fps | 10 frames/s | 4.2 GB |
| Desktop, NVENC | 5.7K, 60 fps | 21 frames/s | 2.4 GB |
| Desktop, NVENC | 4K, 120 fps | 35 frames/s | 1.4 GB |
| Desktop, libx264 (the default) | 8K, 24 fps | 2.7 frames/s | 11.6 GB |
| Intel Core 3 N355, VAAPI, `--threads 6`, other services running | 8K, 24 fps | 2.2 frames/s | 2.6 GB |
| Intel Core 3 N355, VAAPI, `--width 1920` | 5.7K, 60 fps | 15 frames/s | 0.7 GB |

At 8K, the stitch is the slowest step. With libx264, the encoder is the slowest step. The `--width 1920`
row and the peak memory of the N355 were measured with the lens table of version 0.0.1; the grid
stitch is faster.
