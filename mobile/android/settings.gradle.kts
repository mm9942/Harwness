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
        google {
            content { excludeGroup("com.google.zxing") }
        }
        mavenCentral()
    }
}

rootProject.name = "Harw"
include(":app")
