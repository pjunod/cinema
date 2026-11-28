pluginManagement {
    repositories {
        google {
            content {
                includeGroupByRegex("com\\.android.*")
                includeGroupByRegex("com\\.google.*")
                includeGroupByRegex("androidx.*")
            }
        }
        mavenCentral()
        gradlePluginPortal()
    }
}
plugins {
    id("org.gradle.toolchains.foojay-resolver-convention") version "0.10.0"
}
dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories {
        // The large, pinned Robolectric test SDK can hit Central's rate limit.
        maven("https://maven-central.storage-download.googleapis.com/maven2/") {
            content {
                includeVersion("org.robolectric", "android-all-instrumented", "15-robolectric-13954326-i7")
            }
        }
        google()
        mavenCentral()
    }
}

rootProject.name = "plurx"
include(":app", ":baselineprofile")
