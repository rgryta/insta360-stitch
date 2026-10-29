# Library

The core does not decode images, encode images or run ffmpeg. It reads the trailer from a
seekable source and stitches decoded pixels. For a stabilized video, `FrameStitcher` stitches
each decoded frame with its own rotation (Rust and the CLI only for now). The core also makes
remap planes that you give to your own ffmpeg, for a video without stabilization. The Node.js and
Python packages have only the remap planes for now. Thus, the core stays small, and your
application keeps its image pipeline and its transcode settings.

## Node.js

```js
import sharp from 'sharp';
import { Insta360File } from '@cradgear/insta360-stitch';

const path = 'IMG_20261023_053237_00_253.insp';
const file = await Insta360File.open(path); // Does not stop the event loop
if (file.stitchable) {
  const { data, info } = await sharp(path).raw().toBuffer({ resolveWithObject: true });
  const stitched = await file.stitchPhoto(data, info.width, info.height, info.channels); // Does not stop the event loop
  await sharp(stitched, { raw: info }).jpeg({ quality: 95 }).toFile('out.jpg');
} else {
  console.log(file.unsupportedReason);
}
```

`await file.preview()` gives the small equirectangular preview that the camera stitched, as
`{ width, height, rgb }`. Use it as a poster or a thumbnail, or when `file.stitchable` is false.
`file.previewSize` is `null` if the file has no preview. Since version 0.0.2, `preview()` gives a
Promise, as the other functions that read the file. In version 0.0.1, it gave the image directly.

```js
const { width, height, rgb } = await file.preview();
await sharp(rgb, { raw: { width, height, channels: 3 } }).jpeg().toFile('poster.jpg');
```

For a video, `await file.videoMaps(lensWidth, lensHeight)` gives five planes. The default output
width is `2 * lensWidth`. Give the planes to ffmpeg as raw inputs with `FFMPEG_FILTER`. Then use
`await addSphericalMetadata(ffmpegOutput, finalOutput)` to add the 360 video metadata. Do not use
`-movflags +faststart` in ffmpeg, because `addSphericalMetadata` also moves the `moov` box to the
start. `addSphericalMetadata` refuses a fragmented MP4 file (`-movflags frag_keyframe`). It writes a
temporary file next to the output and renames it. Thus, after an error, the output did not change.

Each stitch uses one thread of the libuv thread pool until it is complete. The pool has 4 threads.
To make it larger, set `UV_THREADPOOL_SIZE`.

## Python

```python
from PIL import Image
from insta360_stitch import Insta360File, UnsupportedError

file = Insta360File("IMG_20261023_053237_00_253.insp")
source = Image.open("IMG_20261023_053237_00_253.insp").convert("RGB")
stitched = file.stitch_photo(source.tobytes(), *source.size)  # Releases the GIL
Image.frombytes("RGB", source.size, stitched).save("out.jpg", quality=95)
```

Each heavy operation also has an async form: `Insta360File.open_async`, `stitch_photo_async`,
`stitch_photo_band_async`, `video_maps_async` and `add_spherical_metadata_async`. These are native
coroutines. They do the work on a different thread, thus the event loop does not wait:

```python
stitched = await file.stitch_photo_async(source.tobytes(), *source.size)
```

Each async call starts its own thread and allocates its own output. Thus, `asyncio.gather` over
many photos stitches all of them at the same time and needs memory for all of them. To limit the
memory, use an `asyncio.Semaphore`. If you cancel an async call, the work continues to the end on
its thread, and the result is not used. Do not change its input buffer until that work is complete.

The sync forms release the GIL. Thus, you can also use them from many threads at the same time.

The Python package supports free-threaded Python 3.14t and later. In free-threaded Python, the
module does not turn the GIL on. Free-threaded Python 3.13t is not supported, because PyO3 does not
support it. For all other Python versions from 3.11, one `abi3` wheel is sufficient.

The stitch functions of the Python package accept `bytes`, `bytearray`, `memoryview` and C-contiguous
`uint8` arrays, for example NumPy arrays. They read the data without a copy. Do not change the data
while the function runs.

## Kotlin

The Kotlin Multiplatform library is `eu.gryta:insta360-stitch`. It comes to Maven Central with the first release
after version 0.0.2. Until then, build it from the source (see [Development](development.md)). It has these targets: the JVM (Linux,
macOS and Windows, x64 and arm64) and Android 16 or later (arm64-v8a, and x86_64 for the emulator). The slow
operations are `suspend` functions. They run on `Dispatchers.IO`.

```kotlin
// build.gradle.kts
dependencies { implementation("eu.gryta:insta360-stitch:<version>") }
```

```kotlin
import eu.gryta.insta360stitch.Insta360File

Insta360File.open("IMG_20261023_053237_00_253.insp").use { file ->
    if (file.stitchable) {
        val stitched = file.stitchPhoto(rgb, width, height, 3) // Decoded pixels in, stitched pixels out
    } else {
        println(file.unsupportedReason)
    }
}
```

An `Insta360File` holds native memory. Close it after use (`use {}`). A call after `close()` throws
`IllegalStateException`. `close()` waits for the calls that run on other threads. If you do not close a file, a
`java.lang.ref.Cleaner` frees the memory after the garbage collector finds the file.

If you cancel a coroutine, the native work continues to the end, and the result is not used. The cancelled call
returns only when the native work is complete. A `cancel()` of a Java future does not stop the native work either.

### Android

Android apps often have a `content://` URI and no file path. Open the file from its descriptor. The library keeps its
own duplicate of the descriptor, thus you can close yours after `open`. The library does not move the file position of
the descriptor, thus you can also decode the image from the same descriptor. The file must allow random access: a
descriptor of a pipe gives `IOException`.

```kotlin
val file = contentResolver.openFileDescriptor(uri, "r")!!.use { Insta360File.open(it) }
```

`addSphericalMetadata` needs file paths, also on Android. Give it files in the cache folder of the app.

To stitch a full-resolution photo, use the `Bitmap` form. It reads and writes the pixels of the bitmaps directly, thus
no pixel buffer is on the Java heap:

```kotlin
val source = BitmapFactory.decodeFile(path, BitmapFactory.Options().apply { inPreferredConfig = Bitmap.Config.ARGB_8888 })
val stitched = Insta360File.open(path).use { it.stitchPhoto(source) }
source.recycle()
FileOutputStream(output).use { stitched.compress(Bitmap.CompressFormat.JPEG, 95, it) }
```

The source must be an opaque `ARGB_8888` software bitmap. A different config, a `HARDWARE` bitmap and a recycled bitmap
give `IllegalArgumentException`. A full-resolution X5 photo (11904 x 5952) needs approximately 570 MB of native memory:
the source bitmap (271 MB), the output bitmap (271 MB) and the work memory of the stitch. In an Android 16 emulator
with 2 GB of memory, the stitch of such a photo took 8.5 s. The pixels of a bitmap are not on the Java heap, thus the
heap limit of the app does not apply to them.

## Java

Maven and plain Java projects use the JVM artifact `eu.gryta:insta360-stitch-jvm`. Each slow operation has a blocking
form and a `CompletableFuture` form. The future forms run on a shared thread pool, or on the `Executor` that you give
as the last argument.

```java
import eu.gryta.insta360stitch.Insta360File;
import eu.gryta.insta360stitch.Insta360Stitch;

try (Insta360File file = Insta360File.open("IMG_20261023_053237_00_253.insp")) { // throws IOException
    if (file.isStitchable()) {
        byte[] stitched = file.stitchPhoto(rgb, width, height, 3);
        CompletableFuture<byte[]> later = file.stitchPhotoAsync(rgb, width, height, 3);
    }
}
Insta360Stitch.addSphericalMetadata("ffmpeg-output.mp4", "final.mp4");
```

`Size`, `PreviewImage` and `VideoMaps` are Java records (`maps.width()`). Their `equals` compares the byte arrays by
identity.

The jar contains the native library for each desktop platform. At the first use, the library copies it to the
temporary folder (`java.io.tmpdir`) and loads it. Java 24 and later show a warning for this. To stop the warning,
start Java with `--enable-native-access=ALL-UNNAMED`. If the temporary folder is read-only or does not let programs run
(`noexec`), put the library in a different folder and start Java with
`-Dinsta360stitch.library=<path to the library file>`.

## Bands and memory

To keep less of the output in memory, stitch a photo in bands of rows: `stitch_photo_band` or
`stitch_photo_rows` in Rust, `stitchPhotoBand` in Node.js, Kotlin and Java, or `stitch_photo_band` in Python. Each
band call also lets you show progress. The CLI stitches 64 rows at a time and gives them directly
to the JPEG encoder.

The colour fusion and the seam flow use the full photo, thus they are calculated one time for each
photo. In Rust, make a `PhotoSeam` one time and give it to `stitch_photo_band_with` for each band
(`stitch_photo_band` calculates it again in each call). In Node.js, Python and Kotlin, the file object
keeps the seam of the last photo that it stitched, for the next band of the same photo.

The stitch uses all processor cores. To use fewer cores, set the `RAYON_NUM_THREADS` environment
variable before the process starts (the CLI has `--threads`).

## Errors

Each API has three types of error:

- **Unsupported**: The library cannot stitch the file, because of the camera, the lens accessory
  or the trailer layout. A retry will not help. Show the preview from the camera instead.
- **I/O**: A read operation failed, or it gave fewer bytes than necessary. A retry can help.
- **Invalid argument**: The calling code gave a wrong value, for example a buffer of the wrong size
  or a band outside the photo. Correct the calling code.

| | Unsupported | I/O | Invalid argument |
|---|---|---|---|
| Node.js (message starts with) | `unsupported:` | `i/o error:` | `invalid argument:` |
| Python | `UnsupportedError` | `OSError` | `ValueError` |
| Rust | `Error::Unsupported` | `Error::Io` | `Error::InvalidArgument` |
| Kotlin and Java | `Insta360UnsupportedException` | `java.io.IOException` | `IllegalArgumentException` |

In Python, `UnsupportedError` is also a `ValueError`. Thus, catch `UnsupportedError` before
`ValueError`.

Use only complete files. If a copy operation did not finish, the end of the file is not there.
Then the library gives "unsupported", because it cannot find the trailer.

## Platforms

| Platform | CLI | Node.js | Python | Kotlin and Java |
|---|---|---|---|---|
| Android 16 and later (arm64, x86_64) | - | - | - | Yes |
| Linux x64 and arm64 (glibc) | Yes | Yes | Yes (manylinux) | Yes (JVM) |
| Linux x64 and arm64 (musl) | - | - | Yes (musllinux) | - |
| macOS Intel and Apple silicon | Yes | Yes | Yes (universal2) | Yes (JVM) |
| Windows x64 and arm64 | Yes | Yes | Yes | Yes (JVM) |

Before each release, the release workflow tests the CLI, the Node.js package and the Python package
on each platform in the table. The release builds for Linux use glibc 2.17. Thus, they also run on older
distributions, for example Debian. For a different platform, build the package from the source
code. You need a Rust toolchain for this.
