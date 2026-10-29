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
