pluginManagement {
    repositories {
        google()
        // Central rejects or rate-limits CI runners. Google's Central mirror serves the same
        // files, still checksum-verified; Central is only tried for modules the mirror lacks.
        maven("https://maven-central.storage-download.googleapis.com/maven2/") {
            name = "mavenCentralGoogleMirror"
        }
        mavenCentral()
        gradlePluginPortal()
    }
}

dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories {
        google()
        // Central rejects or rate-limits CI runners. Google's Central mirror serves the same
        // files, still checksum-verified; Central is only tried for modules the mirror lacks.
        maven("https://maven-central.storage-download.googleapis.com/maven2/") {
            name = "mavenCentralGoogleMirror"
        }
        mavenCentral()
        exclusiveContent {
            forRepository {
                maven("https://jitpack.io") {
                    name = "jitpackSpake2"
                }
            }
            filter {
                includeGroup("com.github.MuntashirAkon.spake2-java")
            }
        }
    }
}

rootProject.name = "PeppyMobile"
include(":app")
include(":jvm-smoke")
include(":libadb")
