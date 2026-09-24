import javax.inject.Inject
import org.gradle.api.DefaultTask
import org.gradle.api.file.DirectoryProperty
import org.gradle.api.file.FileSystemOperations
import org.gradle.api.tasks.InputDirectory
import org.gradle.api.tasks.OutputDirectory
import org.gradle.api.tasks.TaskAction

plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.compose)
    alias(libs.plugins.kotlin.serialization)
}

abstract class GenerateReaderAssets @Inject constructor(
    private val fileSystem: FileSystemOperations,
) : DefaultTask() {
    @get:InputDirectory
    abstract val sourceDirectory: DirectoryProperty

    @get:OutputDirectory
    abstract val outputDirectory: DirectoryProperty

    @TaskAction
    fun generate() {
        fileSystem.sync {
            from(sourceDirectory)
            include("reader.js", "offline-reader.js", "offline-reader.html")
            into(outputDirectory)
        }
    }
}

val generateReaderAssets = tasks.register<GenerateReaderAssets>("generateReaderAssets") {
    sourceDirectory.set(layout.projectDirectory.dir("../../../crates/plurxd/src/web"))
    outputDirectory.set(layout.buildDirectory.dir("generated/reader-assets"))
}

/**
 * True when this Gradle invocation asked for a release task.
 *
 * `signingConfigs { }` and `buildTypes { }` are both evaluated during
 * configuration, on every invocation — including `make android` and
 * `make android-test`, which run on machines that hold no signing material
 * and must keep working. So the requirement is keyed on the requested task
 * names rather than on the block being reached.
 *
 * Every Gradle entry point in this repository names its tasks explicitly
 * (`Makefile`: `:app:assembleDebug`, `testDebugUnitTest lintDebug`,
 * `assembleDebug assembleDebugAndroidTest`, `:app:assembleRelease`;
 * `scripts/ship-physical`: `:app:assembleRelease`), so no supported path
 * packages the release variant without a "Release" task name;
 * tests/operations/test_android_credential_exposure.py scans the Makefile and
 * scripts/ to keep that true.
 */
val releaseTaskRequested: Boolean =
    gradle.startParameter.taskNames.any { it.contains("Release") }

/**
 * Resolve one piece of release signing material, or fail the build naming it.
 *
 * The signing material never enters the repository: it is streamed into the
 * build environment from the fleet vault, the same way the Forgejo registry
 * token is. There is deliberately no default and no fallback to the debug
 * keystore. A debug-signed "release" installs happily on a test device, so the
 * mistake stays invisible until the first properly signed build refuses to
 * upgrade it in place and every device in the fleet needs an uninstall first;
 * Play refuses such an artifact outright. Failing here, while the mistake
 * costs one shell variable, is the cheap moment.
 */
fun requiredSigningValue(name: String): String =
    (project.findProperty(name) as String? ?: System.getenv(name))
        ?.takeIf { it.isNotBlank() }
        ?: error(
            "release signing is not configured: $name is unset. " +
                "assembleRelease needs PLURX_ANDROID_KEYSTORE, " +
                "PLURX_ANDROID_KEYSTORE_PASSWORD, PLURX_ANDROID_KEY_ALIAS and " +
                "PLURX_ANDROID_KEY_PASSWORD; see docs/PUBLISHING.md section 5.1. " +
                "Use `make android` for a local debug build instead."
        )

android {
    namespace = "tv.plurx.app"
    compileSdk = 37

    defaultConfig {
        applicationId = "tv.plurx.app"
        // 23 covers phones and the vast majority of Android TV / Google TV boxes.
        minSdk = 23
        targetSdk = 37
        versionCode = 121
        versionName = "0.3.0"
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
    }

    signingConfigs {
        create("release") {
            // Populated only when a release task was requested: reading the
            // four variables unconditionally would fail every debug build on
            // a machine without the key, and `signingConfigs { }` is
            // evaluated on every invocation. The guard is exact for this
            // repository because every Gradle entry point names its tasks
            // (see `releaseTaskRequested`), and an operations test keeps that
            // true.
            if (releaseTaskRequested) {
                storeFile = file(requiredSigningValue("PLURX_ANDROID_KEYSTORE"))
                storePassword = requiredSigningValue("PLURX_ANDROID_KEYSTORE_PASSWORD")
                keyAlias = requiredSigningValue("PLURX_ANDROID_KEY_ALIAS")
                keyPassword = requiredSigningValue("PLURX_ANDROID_KEY_PASSWORD")
            }
        }
    }

    buildTypes {
        create("capabilityProbe") {
            initWith(getByName("debug"))
            applicationIdSuffix = ".capabilityprobe"
            versionNameSuffix = "-capability-probe"
            matchingFallbacks += listOf("debug")
        }
        release {
            // `material-icons-extended` alone is several thousand vector assets
            // of which this app draws about twenty, and every library it pulls
            // in ships code for surfaces the viewer never opens. R8 removes
            // what nothing reaches; `shrinkResources` removes the drawables and
            // strings that go with it. `proguard-rules.pro` had been dead
            // configuration since the day it was written.
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")
            // The upload key, never the debug key. `release` already clears
            // `debuggable`, which is what `adb shell run-as` follows; the
            // signer is a separate property and this is it.
            signingConfig = signingConfigs.getByName("release")
        }
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    buildFeatures {
        buildConfig = true
        compose = true
    }
    sourceSets {
        // Shared JVM resources: player-input-contract.json and
        // playback-info-fields.json are consumed directly from tests/playback.
        getByName("test").resources.directories.add("../../../tests/contracts")
        getByName("test").resources.directories.add("../../../tests/playback")
    }
}

androidComponents {
    onVariants(selector().all()) { variant ->
        variant.sources.assets?.addGeneratedSourceDirectory(
            generateReaderAssets,
            GenerateReaderAssets::outputDirectory,
        )
    }
}

dependencies {
    implementation(libs.androidx.core.ktx)
    implementation(libs.androidx.activity.compose)
    implementation(platform(libs.androidx.compose.bom))
    implementation(libs.androidx.ui)
    implementation(libs.androidx.ui.graphics)
    implementation(libs.androidx.ui.tooling.preview)
    implementation(libs.androidx.material3)
    implementation(libs.androidx.material.icons)
    implementation(libs.androidx.lifecycle.runtime.compose)
    implementation(libs.androidx.lifecycle.viewmodel.compose)
    implementation(libs.androidx.navigation.compose)
    implementation(libs.kotlinx.coroutines.android)
    implementation(libs.kotlinx.serialization.json)

    implementation(libs.media3.exoplayer)
    implementation(libs.media3.exoplayer.hls)
    implementation(libs.media3.ui)
    implementation(libs.media3.session)
    implementation(libs.media3.datasource.okhttp)

    implementation(libs.retrofit)
    implementation(libs.retrofit.serialization)
    implementation(libs.okhttp)
    implementation(libs.okhttp.logging)

    implementation(libs.coil.compose)
    implementation(libs.datastore.preferences)
    implementation(libs.google.code.scanner)

    testImplementation(libs.junit)
    // The reporter's rules are about ordering and backoff, so its tests drive
    // a virtual clock rather than waiting out real seconds.
    testImplementation(libs.kotlin.test.junit)
    testImplementation(libs.kotlinx.coroutines.test)
    androidTestImplementation(libs.androidx.test.ext.junit)
    androidTestImplementation(libs.androidx.test.runner)
    androidTestImplementation(libs.androidx.test.espresso)
    androidTestImplementation(platform(libs.androidx.compose.bom))
    androidTestImplementation(libs.androidx.ui.test.junit4)
    debugImplementation(libs.androidx.ui.test.manifest)
}
