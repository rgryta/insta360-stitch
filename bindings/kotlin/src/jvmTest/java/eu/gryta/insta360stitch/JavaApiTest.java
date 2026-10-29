package eu.gryta.insta360stitch;

import static org.junit.jupiter.api.Assertions.*;

import java.io.IOException;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.ExecutionException;
import java.util.concurrent.Executors;
import org.junit.jupiter.api.Test;

/** The library as a Java user sees it: blocking calls, futures, try-with-resources and checked exceptions. */
class JavaApiTest {
    private static String syntheticX5() {
        return TestSupport_jvmKt.tempFile("IMG_1.insp", FixturesKt.getSYNTHETIC_X5());
    }

    @Test
    void blockingFutureAndTryWithResources() throws Exception {
        byte[] pattern = new byte[64 * 32 * 3];
        try (Insta360File file = Insta360File.open(syntheticX5())) {
            assertTrue(file.isStitchable());
            assertNull(file.getUnsupportedReason());
            assertEquals(new Size(2, 2), file.getPreviewSize());
            byte[] whole = file.stitchPhoto(pattern, 64, 32, 3);
            assertEquals(pattern.length, whole.length);
            assertArrayEquals(whole, file.stitchPhotoAsync(pattern, 64, 32, 3).get());
            var executor = Executors.newSingleThreadExecutor();
            assertArrayEquals(whole, file.stitchPhotoAsync(pattern, 64, 32, 3, executor).get());
            executor.shutdown();
            assertEquals(7 * 64 * 3, file.stitchPhotoBand(pattern, 64, 32, 3, 10, 7).length);
            VideoMaps maps = file.videoMaps(64, 64);
            assertEquals(128, maps.width());
            assertEquals(64, file.videoMaps(64, 64, 64).width());
            assertEquals(2, file.preview().width());
        }
    }

    @Test
    void checkedIoExceptionAndFutures() {
        // IOException is a checked exception for Java: the compiler makes the caller handle it.
        assertThrows(IOException.class, () -> Insta360File.open("/nonexistent/IMG_1.insp"));
        ExecutionException e = assertThrows(ExecutionException.class, () -> Insta360File.openAsync("/nonexistent/x").get());
        assertInstanceOf(IOException.class, e.getCause());
        assertThrows(IOException.class, () -> Insta360Stitch.addSphericalMetadata("/nonexistent/a.mp4", "/nonexistent/b.mp4"));
    }

    @Test
    void moduleFunctionsAndRecords() {
        assertTrue(Insta360Stitch.FFMPEG_FILTER.contains("maskedmerge"));
        assertEquals(480, Insta360Stitch.cameraUtcOffsetMinutes("20260821105307", 1_787_280_787L));
        assertNull(Insta360Stitch.cameraUtcOffsetMinutes("20260821050207", 1_787_280_787L));
        assertTrue(Size.class.isRecord());
        assertTrue(PreviewImage.class.isRecord());
        assertTrue(VideoMaps.class.isRecord());
    }

    @Test
    void cancelledFutureDoesNotBreakTheFile() throws Exception {
        byte[] big = new byte[2048 * 1024 * 3];
        try (Insta360File file = Insta360File.open(syntheticX5())) {
            CompletableFuture<byte[]> future = file.stitchPhotoAsync(big, 2048, 1024, 3);
            future.cancel(true);
            assertEquals(64 * 32 * 3, file.stitchPhoto(new byte[64 * 32 * 3], 64, 32, 3).length);
        }
    }

    @Test
    void noSuspendFormIsVisibleToJava() {
        // javac does not show synthetic methods to Java code. The suspend forms must be synthetic.
        for (Class<?> type : new Class<?>[] {Insta360File.class, Insta360File.Companion.class, Insta360Stitch.class}) {
            for (var method : type.getMethods()) {
                for (var parameter : method.getParameterTypes()) {
                    if (parameter.getName().equals("kotlin.coroutines.Continuation")) {
                        assertTrue(method.isSynthetic(), method.toString());
                    }
                }
            }
        }
    }
}
