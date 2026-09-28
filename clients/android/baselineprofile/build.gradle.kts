plugins { alias(libs.plugins.android.test) }

android {
    namespace = "tv.plurx.profile"
    compileSdk = 37
    targetProjectPath = ":app"
    // The harness must survive cold-killing the separate measured app process.
    experimentalProperties["android.experimental.self-instrumenting"] = true
    defaultConfig {
        minSdk = 28
        targetSdk = 37
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
    }
    buildTypes {
        create("profileCapture") {
            isDebuggable = true
            signingConfig = signingConfigs.getByName("debug")
        }
        create("release") {
            // The instrumentation APK can be debugged; the target remains the
            // signed, R8-optimized, non-debuggable app release.
            isDebuggable = true
            // Separate self-instrumenting harness only, never the measured APK.
            signingConfig = signingConfigs.getByName("debug")
        }
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}

androidComponents {
    beforeVariants(selector().all()) { variant ->
        // Never collect or credit a debug app journey.
        variant.enable = variant.buildType == "release" || variant.buildType == "profileCapture"
    }
}

dependencies {
    implementation(libs.androidx.benchmark.macro)
    implementation(libs.androidx.test.ext.junit)
    implementation(libs.androidx.test.runner)
    implementation(libs.androidx.uiautomator)
}
