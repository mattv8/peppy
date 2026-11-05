pluginManagement {
    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
}

dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories {
        google()
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
