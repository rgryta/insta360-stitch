// Checks the Kotlin/Java library jar on the platform where it runs. The native library must come from the jar.
// Usage: java -cp "<folder with the jar and its dependencies>/*" scripts/VerifyJvm.java <synthetic_x5.insp>
import eu.gryta.insta360stitch.Insta360File;
import eu.gryta.insta360stitch.Insta360Stitch;
import java.io.IOException;

public class VerifyJvm {
    public static void main(String[] args) throws Exception {
        if (System.getProperty("insta360stitch.library") != null) {
            throw new AssertionError("insta360stitch.library is set: the check must use the library in the jar");
        }
        try (Insta360File file = Insta360File.open(args[0])) {
            check(file.isStitchable(), "the synthetic file is not stitchable");
            byte[] stitched = file.stitchPhoto(new byte[64 * 32 * 3], 64, 32, 3);
            check(stitched.length == 64 * 32 * 3, "the stitch gave " + stitched.length + " bytes");
        }
        check(Insta360Stitch.FFMPEG_FILTER.contains("maskedmerge"), "FFMPEG_FILTER");
        try {
            Insta360File.open(args[0] + ".missing");
            throw new AssertionError("no IOException for a missing file");
        } catch (IOException expected) {
            // A missing file is an I/O error.
        }
        System.out.println("VerifyJvm: OK");
    }

    private static void check(boolean condition, String message) {
        if (!condition) throw new AssertionError(message);
    }
}
