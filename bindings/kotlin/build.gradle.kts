import org.jetbrains.kotlin.gradle.ExperimentalKotlinGradlePluginApi
import org.jetbrains.kotlin.gradle.dsl.JvmTarget
import org.jetbrains.kotlin.gradle.plugin.KotlinPlatformType

plugins {
    alias(libs.plugins.kotlinMultiplatform)
    alias(libs.plugins.android.kotlin.multiplatform.library)
}

// The repository root. The Rust workspace and the test fixtures are there.
val repoRoot: File = rootDir.resolve("../..").canonicalFile

group = "eu.gryta"
// The version comes from the root Cargo.toml ([workspace.package] version). Thus, there is no second version number.
version = Regex("""(?m)^version = "([^"]+)"""")
    .find(repoRoot.resolve("Cargo.toml").readText())
    ?.groupValues?.get(1)
    ?: error("no version in Cargo.toml")

// The test fixture as a Kotlin constant: the common tests then do not need file access to the repository.
val generateTestFixtures = tasks.register("generateTestFixtures") {
    val input = repoRoot.resolve("tests/fixtures/synthetic_x5.insp")
    val output = layout.buildDirectory.file("generated/fixtures/Fixtures.kt")
    inputs.file(input)
    outputs.dir(output.map { it.asFile.parentFile })
    doLast {
        val bytes = input.readBytes().joinToString(",") { it.toString() }
        output.get().asFile.writeText(
            "// Generated from tests/fixtures/synthetic_x5.insp. Do not edit.\n" +
                "package eu.gryta.insta360stitch\n\n" +
                "internal val SYNTHETIC_X5: ByteArray = byteArrayOf($bytes)\n",
        )
    }
}

// The version for the name of the extracted native library (see NativeLoader).
val generateVersion = tasks.register("generateVersion") {
    val text = "// Generated from the version in Cargo.toml. Do not edit.\n" +
        "package eu.gryta.insta360stitch\n\ninternal const val VERSION = \"$version\"\n"
    val output = layout.buildDirectory.file("generated/version/Version.kt")
    inputs.property("text", text)
    outputs.dir(output.map { it.asFile.parentFile })
    doLast { output.get().asFile.writeText(text) }
}

// The static libraries for iOS: from -Pinsta360.iosNativeDir=<dir> (<dir>/<rust target>/libinsta360_stitch_jni.a),
// else cargo builds them (macOS only).
val iosNativeDir = providers.gradleProperty("insta360.iosNativeDir").map { rootDir.resolve(it) }
val cargoBuildIos = tasks.register<Exec>("cargoBuildIos") {
    workingDir = repoRoot
    commandLine(
        "cargo", "build", "--release", "-p", "insta360-stitch-jni",
        "--target", "aarch64-apple-ios", "--target", "aarch64-apple-ios-sim",
    )
}
if (!iosNativeDir.isPresent) {
    tasks.matching { it.name.startsWith("cinteropInsta360_stitchIos") }.configureEach { dependsOn(cargoBuildIos) }
}

kotlin {
    jvmToolchain(25)

    @OptIn(ExperimentalKotlinGradlePluginApi::class)
    compilerOptions {
        freeCompilerArgs.add("-Xexpect-actual-classes")
    }

    jvm {
        compilerOptions {
            // Java 21 bytecode, and only the Java 21 API.
            jvmTarget.set(JvmTarget.JVM_21)
            freeCompilerArgs.add("-Xjdk-release=21")
        }
    }

    android {
        namespace = "eu.gryta.insta360stitch"
        compileSdk = 37
        minSdk = 36
        compilerOptions {
            jvmTarget.set(JvmTarget.JVM_21)
        }
        // The apps that use the library get these R8 rules: JNI finds the classes by name.
        optimization {
            consumerKeepRules.publish = true
            consumerKeepRules.files.add(file("consumer-rules.pro"))
        }
        // The common tests also run on a device or an emulator.
        withDeviceTestBuilder { sourceSetTreeName = "test" }.configure {
            instrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
        }
    }

    // iOS: the C API of the bridge through cinterop, with the static library for each target. On Linux and Windows,
    // Gradle skips these targets.
    for ((target, rustTarget) in listOf(iosArm64() to "aarch64-apple-ios", iosSimulatorArm64() to "aarch64-apple-ios-sim")) {
        val libraryDir = iosNativeDir.map { it.resolve(rustTarget) }.getOrElse(repoRoot.resolve("target/$rustTarget/release"))
        target.compilations.getByName("main").cinterops.create("insta360_stitch") {
            definitionFile.set(file("src/nativeInterop/cinterop/insta360_stitch.def"))
            includeDirs(repoRoot.resolve("bindings/kotlin/native/include"))
            extraOpts("-libraryPath", libraryDir.path)
        }
    }

    // jvmAndroid: the JNI declarations and the Java forms, shared by the JVM and Android.
    @OptIn(ExperimentalKotlinGradlePluginApi::class)
    applyDefaultHierarchyTemplate {
        common {
            group("jvmAndroid") {
                withCompilations {
                    it.target.platformType in setOf(KotlinPlatformType.jvm, KotlinPlatformType.androidJvm)
                }
            }
        }
    }

    sourceSets {
        commonMain.dependencies {
            implementation(libs.kotlinx.coroutines.core)
        }
        commonTest {
            // The task output as a source folder: Gradle then runs the task before each compile that needs it.
            kotlin.srcDir(generateTestFixtures)
            dependencies {
                implementation(libs.kotlin.test)
                implementation(libs.kotlinx.coroutines.test)
            }
        }
        jvmMain {
            kotlin.srcDir(generateVersion)
        }
        getByName("androidDeviceTest").dependencies {
            implementation(libs.androidx.test.runner)
            implementation(libs.androidx.test.ext.junit)
        }
        jvmTest.dependencies {
            implementation(libs.junit.jupiter)
            runtimeOnly(libs.junit.platform.launcher)
        }
    }
}

tasks.withType<JavaCompile>().configureEach {
    options.release.set(21)
}



// The jar holds one JNI library for each desktop platform, from -Pinsta360.nativeDir=<dir> (<dir>/<os>-<arch>/<file>).
// The release sets -Pinsta360.allNatives=true: then a missing platform stops the build.
val desktopPlatforms = listOf("linux-x86_64", "linux-aarch64", "macos-x86_64", "macos-aarch64", "windows-x86_64", "windows-aarch64")
val nativeDir = providers.gradleProperty("insta360.nativeDir").map { rootDir.resolve(it) }
val allNatives = providers.gradleProperty("insta360.allNatives").map { it.toBoolean() }.orElse(false)
tasks.named<Jar>("jvmJar") {
    if (nativeDir.isPresent) {
        val dir = nativeDir.get()
        from(dir) { into("eu/gryta/insta360stitch/native") }
        // Checked when Gradle configures the build: a cached jar must not hide a missing platform.
        if (allNatives.get()) {
            val missing = desktopPlatforms.filter { dir.resolve(it).listFiles().isNullOrEmpty() }
            check(missing.isEmpty()) { "no native library for: ${missing.joinToString()}" }
        }
    }
    from(repoRoot) {
        include("LICENSE-MIT", "LICENSE-APACHE", "NOTICE")
        into("META-INF")
    }
}

// The jar and its runtime dependencies in one folder, for scripts/VerifyJvm.java.
tasks.register<Sync>("jvmRuntimeJars") {
    from(tasks.named("jvmJar"))
    from(configurations.named("jvmRuntimeClasspath"))
    into(layout.buildDirectory.dir("jvm-runtime"))
}

// The JNI library for the host, for the JVM tests. A release build, as the Node.js tests use: a debug stitch is slow.
val cargoBuildHost = tasks.register<Exec>("cargoBuildHost") {
    workingDir = repoRoot
    commandLine("cargo", "build", "--release", "-p", "insta360-stitch-jni")
}
val os: String = System.getProperty("os.name").lowercase()
val hostLibrary: File = repoRoot.resolve(
    "target/release/" + when {
        os.startsWith("windows") -> "insta360_stitch_jni.dll"
        os.startsWith("mac") -> "libinsta360_stitch_jni.dylib"
        else -> "libinsta360_stitch_jni.so"
    },
)

// The JNI libraries for Android: from -Pinsta360.androidJniDir=<dir> (<dir>/<abi>/libinsta360_stitch_jni.so), else
// cargo-ndk builds them (it needs the NDK, ANDROID_NDK_HOME). The AAR takes them from src/androidMain/jniLibs.
val androidAbis = listOf("arm64-v8a", "x86_64")
val androidJniDir = providers.gradleProperty("insta360.androidJniDir").map { rootDir.resolve(it) }
val cargoBuildAndroid = tasks.register<Exec>("cargoBuildAndroid") {
    workingDir = repoRoot
    commandLine(
        listOf("cargo", "ndk") + androidAbis.flatMap { listOf("-t", it) } +
            listOf("--platform", "36", "-o", "target/android-jni", "build", "-p", "insta360-stitch-jni", "--release"),
    )
}
val androidJniLibs = tasks.register<Sync>("androidJniLibs") {
    if (androidJniDir.isPresent) {
        from(androidJniDir)
    } else {
        from(repoRoot.resolve("target/android-jni"))
        dependsOn(cargoBuildAndroid)
    }
    include(androidAbis.map { "$it/libinsta360_stitch_jni.so" })
    into(layout.projectDirectory.dir("src/androidMain/jniLibs"))
}
tasks.matching { it.name.endsWith("JniLibFolders") || it.name.startsWith("copyAndroidMainJniLibs") }
    .configureEach { dependsOn(androidJniLibs) }

tasks.named<Test>("jvmTest") {
    dependsOn(cargoBuildHost)
    // A new library must run the tests again; without this input, Gradle uses the cached result.
    inputs.file(hostLibrary).withPropertyName("hostLibrary").withPathSensitivity(PathSensitivity.NONE)
    useJUnitPlatform()
    systemProperty("insta360stitch.library", hostLibrary.path)
    System.getenv("INSTA360_FIXTURES")?.let { environment("INSTA360_FIXTURES", it) }
    testLogging {
        exceptionFormat = org.gradle.api.tasks.testing.logging.TestExceptionFormat.FULL
        events("passed", "failed", "skipped")
    }
}
