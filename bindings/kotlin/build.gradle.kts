import org.jetbrains.kotlin.gradle.ExperimentalKotlinGradlePluginApi
import org.jetbrains.kotlin.gradle.dsl.JvmTarget
import org.jetbrains.kotlin.gradle.plugin.KotlinPlatformType

plugins {
    alias(libs.plugins.kotlinMultiplatform)
}

// The repository root. The Rust workspace and the test fixtures are there.
val repoRoot: File = rootDir.resolve("../..").canonicalFile

group = "eu.gryta"
// The version comes from the root Cargo.toml ([workspace.package] version). Thus, there is no second version number.
version = Regex("""(?m)^version = "([^"]+)"""")
    .find(repoRoot.resolve("Cargo.toml").readText())
    ?.groupValues?.get(1)
    ?: error("no version in Cargo.toml")

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
            kotlin.srcDir(layout.buildDirectory.dir("generated/fixtures/commonTest/kotlin"))
            dependencies {
                implementation(libs.kotlin.test)
                implementation(libs.kotlinx.coroutines.test)
            }
        }
        jvmMain {
            kotlin.srcDir(layout.buildDirectory.dir("generated/version/jvmMain/kotlin"))
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

// The test fixture as a Kotlin constant: the common tests then do not need file access to the repository.
val generateTestFixtures by tasks.registering {
    val input = repoRoot.resolve("tests/fixtures/synthetic_x5.insp")
    val output = layout.buildDirectory.file("generated/fixtures/commonTest/kotlin/Fixtures.kt")
    inputs.file(input)
    outputs.file(output)
    doLast {
        val bytes = input.readBytes().joinToString(",") { it.toString() }
        output.get().asFile.writeText(
            "// Generated from tests/fixtures/synthetic_x5.insp. Do not edit.\n" +
                "package eu.gryta.insta360stitch\n\n" +
                "internal val SYNTHETIC_X5: ByteArray = byteArrayOf($bytes)\n",
        )
    }
}
tasks.matching { it.name.startsWith("compileTestKotlin") || it.name == "compileKotlinMetadata" }
    .configureEach { dependsOn(generateTestFixtures) }

// The version for the name of the extracted native library (see NativeLoader).
val generateVersion by tasks.registering {
    val text = "// Generated from the version in Cargo.toml. Do not edit.\n" +
        "package eu.gryta.insta360stitch\n\ninternal const val VERSION = \"$version\"\n"
    val output = layout.buildDirectory.file("generated/version/jvmMain/kotlin/Version.kt")
    inputs.property("text", text)
    outputs.file(output)
    doLast { output.get().asFile.writeText(text) }
}
tasks.matching { it.name == "compileKotlinJvm" }.configureEach { dependsOn(generateVersion) }

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
val jvmRuntimeJars by tasks.registering(Sync::class) {
    from(tasks.named("jvmJar"))
    from(configurations.named("jvmRuntimeClasspath"))
    into(layout.buildDirectory.dir("jvm-runtime"))
}

// The JNI library for the host, for the JVM tests. A release build, as the Node.js tests use: a debug stitch is slow.
val cargoBuildHost by tasks.registering(Exec::class) {
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
