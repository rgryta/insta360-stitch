# Hardware encoding

A hardware encoder is much faster than libx264 and needs much less memory. At 8K, libx264 needs
more than 4 GB. The commands below are measured; see [Performance](../README.md#performance).

## Intel VAAPI

For an Intel iGPU, for example the server of Immich:

```sh
insta360-stitch video VID.insv out.mp4 --threads 6 --hwaccel vaapi --encoder hevc_vaapi \
  --encoder-arg=-vaapi_device --encoder-arg=/dev/dri/renderD128 \
  --encoder-arg=-vf --encoder-arg=hwupload,scale_vaapi=format=nv12 --encoder-arg=-filter_threads --encoder-arg=1 \
  --encoder-arg=-low_power --encoder-arg=1 --encoder-arg=-async_depth --encoder-arg=1 \
  --encoder-arg=-qp --encoder-arg=22
```

- `-low_power 1` uses the fixed-function encoder. It is 2 times faster than the default encoder
  and needs half the memory (1.1 GB at 8K), with the same quality at the same `-qp`. Some GPUs do
  not have the low-power encoder. Then remove `-low_power`.
- `scale_vaapi=format=nv12` changes the format on the GPU, not on the CPU.
- `-async_depth 1` keeps fewer frames in the encoder: less memory at the same speed.
- On a computer that has other services, `--threads 6` keeps 2 of 8 cores free. On an Intel Core 3
  N355, 6 threads stitch as fast as 8.

## NVIDIA NVENC

```sh
insta360-stitch video VID.insv out.mp4 --hwaccel cuda --encoder hevc_nvenc \
  --encoder-arg=-preset --encoder-arg=p5 --encoder-arg=-tier --encoder-arg=high \
  --encoder-arg=-rc --encoder-arg=vbr --encoder-arg=-cq --encoder-arg=24 --encoder-arg=-b:v --encoder-arg=0 \
  --encoder-arg=-maxrate --encoder-arg=400M --encoder-arg=-bufsize --encoder-arg=800M \
  --encoder-arg=-spatial-aq --encoder-arg=1 --encoder-arg=-temporal-aq --encoder-arg=1
```

- The constant quality mode (`-rc vbr -cq`) needs `-maxrate`. Without it, the bit rate of an 8K
  video stops at approximately 58 Mbit/s, and all `-cq` values give the same file.
- `h264_nvenc` encodes at most 4096 pixels wide. For 8K, use `hevc_nvenc` or `av1_nvenc`.
- At the same file size, `av1_nvenc` gives a slightly better quality than `hevc_nvenc`.

## Decoder memory

The decoder uses 2 threads for each lens stream by default (`--decoder-threads`). More threads do
not make an 8K lens stream faster, but they use much more memory. At 8K with VAAPI, the decoder
needs 1.2 GB with 2 threads and 2.3 GB with the ffmpeg default. With software decode, it needs
0.6 GB with 2 threads and 2.2 GB with the ffmpeg default. On a computer with much memory and a
fast CPU, more threads can make the decode of 5.7K and 4K streams approximately 10% faster.

## Quality of the encoders

Measured on the same 80 frames of an 8K video for each encoder (SSIM against the source frames, higher is
better; speed of the encoder alone):

| Encoder | Speed | Size | SSIM |
|---|---|---|---|
| hevc_vaapi `-low_power 1 -qp 22` (Intel N355) | 16.7 fps | 106 MB | 0.979 |
| hevc_vaapi `-low_power 1 -qp 25` | 16 fps | 69 MB | 0.967 |
| hevc_vaapi `-qp 22`, without `-low_power` | 7.5 fps | 102 MB | 0.980 |
| hevc_qsv `-global_quality 18` | 6.3 fps | 68 MB | 0.961 |
| hevc_nvenc p4 `-cq 22` (RTX 4070 Ti) | 29 fps | 77 MB | 0.975 |
| hevc_nvenc p5, AQ, `-cq 26` | 21 fps | 48 MB | 0.963 |
| av1_nvenc p4 `-cq 34` | 24 fps | 35 MB | 0.950 |
| libx264 veryfast `-crf 20` (24 threads) | 11 fps | 106 MB | 0.977 |
