# Development

```sh
cargo test -p insta360-stitch -p insta360-stitch-cli
cd bindings/node && npm ci && npm run build && npm test
cd bindings/python && uv run pytest
```

For the Kotlin and Java library (`bindings/kotlin`), install JDK 25: the Gradle daemon must run on Java 25
(`gradle/gradle-daemon-jvm.properties`). `./gradlew jvmTest` builds the bridge crate with cargo, then runs the JVM
tests:

```sh
cd bindings/kotlin && ./gradlew jvmTest
```

For the Android part, Gradle needs the Android SDK (`sdk.dir` in `bindings/kotlin/local.properties`, or
`ANDROID_HOME`) with the platform `android-37.0`, the NDK 30.0.16248370 (`ANDROID_NDK_HOME`) and
`cargo install --locked cargo-ndk@4.1.2`. Gradle then builds the JNI libraries with cargo-ndk. To use libraries that you
built before, give `-Pinsta360.androidJniDir=<folder>/<abi>/libinsta360_stitch_jni.so`. The common tests run in an
Android 16 emulator or on a phone:

```sh
cd bindings/kotlin && ./gradlew connectedAndroidDeviceTest
```

Some tests need real camera files. These tests read the files from the folder in
`INSTA360_FIXTURES`. If you do not set `INSTA360_FIXTURES`, the tests do not run.

The documentation and the code comments use Simplified Technical English (ASD-STE100).

## Contributing

To add support for a different camera or lens accessory, two files are necessary:

1. A photo from that camera or with that accessory.
2. The same photo, exported from Insta360 Studio.

These files let us measure the field of view and examine the stitch.
