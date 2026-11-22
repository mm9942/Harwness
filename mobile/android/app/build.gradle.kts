plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.plugin.compose")
}

val uniffiSources = layout.buildDirectory.dir("generated/uniffi")
val mobileLibraries = layout.buildDirectory.dir("generated/jniLibs")

android {
    namespace = "dev.harw.mobile"
    compileSdk {
        version = release(37) { minorApiLevel = 1 }
    }
    ndkVersion = "28.2.13676358"

    defaultConfig {
        applicationId = "dev.harw.mobile"
        minSdk = 28
        targetSdk = 37
        versionCode = 1
        versionName = "0.1.0"
        ndk { abiFilters += "arm64-v8a" }
    }

    buildFeatures { compose = true }

    buildTypes {
        release {
            isMinifyEnabled = true
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    sourceSets.named("main") {
        // Prepared by the native build script before Gradle runs.
        kotlin.directories.add(uniffiSources.get().asFile.absolutePath)
        jniLibs.directories.add(mobileLibraries.get().asFile.absolutePath)
    }

    packaging {
        jniLibs { useLegacyPackaging = false }
        resources { excludes += "/META-INF/{AL2.0,LGPL2.1}" }
    }
}

dependencies {
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.11.0")
    implementation("androidx.core:core-ktx:1.19.1")
    implementation("androidx.activity:activity-compose:1.13.0")
    implementation("androidx.lifecycle:lifecycle-runtime-compose:2.11.0")
    implementation(platform("androidx.compose:compose-bom:2026.11.01"))
    implementation("androidx.compose.foundation:foundation")
    implementation("androidx.compose.ui:ui")
    implementation("androidx.compose.material3:material3")
    implementation("androidx.compose.material:material-icons-extended")
    implementation("net.java.dev.jna:jna:5.19.1@aar")
    implementation("com.google.code.gson:gson:2.11.0")
    implementation("com.google.zxing:core:3.5.4")
    implementation("androidx.camera:camera-core:1.7.0")
    implementation("androidx.camera:camera-camera2:1.7.0")
    implementation("androidx.camera:camera-lifecycle:1.7.0")
    implementation("androidx.camera:camera-view:1.7.0")

    testImplementation("junit:junit:4.13.2")
}

val verifyMobileBridge = tasks.register("verifyMobileBridge") {
    doLast {
        check(uniffiSources.get().asFile.walkTopDown().any { it.isFile && it.extension == "kt" }) {
            "Generate the Rust UniFFI binding before building Harw Android."
        }
        check(mobileLibraries.get().file("arm64-v8a/libharw_mobile_ffi.so").asFile.isFile) {
            "Build the arm64 Rust library before building Harw Android."
        }
    }
}
tasks.named("preBuild") { dependsOn(verifyMobileBridge) }
