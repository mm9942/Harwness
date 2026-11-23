import java.util.Properties

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.plugin.compose")
}

// Firebase (FCM-Push fuer Updates): nur aktiv, wenn die lokale, nicht eingecheckte google-services.json vorhanden ist.
if (file("google-services.json").isFile) apply(plugin = "com.google.gms.google-services")

val uniffiSources = layout.buildDirectory.dir("generated/uniffi")
val mobileLibraries = layout.buildDirectory.dir("generated/jniLibs")

// Stable signing from a local, uncommitted properties file. This build never creates a key.
val localProperties = Properties().apply {
    val file = rootProject.file("local.properties")
    if (file.isFile) file.inputStream().use { load(it) }
}
val signingConfigPath: String? =
    (System.getenv("HARW_SIGNING_CONFIG") ?: localProperties.getProperty("harw.signing.config"))
        ?.takeIf { it.isNotBlank() }

val signingProps: Properties? = signingConfigPath?.let { path ->
    val cfg = File(path)
    if (!cfg.isAbsolute || !cfg.isFile) {
        throw GradleException("harw signing config incomplete: file")
    }
    Properties().apply { cfg.inputStream().use { load(it) } }
}
val repoRoot = rootProject.rootDir.parentFile.parentFile.canonicalFile
val signingStoreFile: File? = signingProps?.let { props ->
    for (key in listOf("storeFile", "storePassword", "keyAlias", "keyPassword")) {
        if (props.getProperty(key).isNullOrBlank()) {
            throw GradleException("harw signing config incomplete: $key")
        }
    }
    val store = File(props.getProperty("storeFile").trim())
    if (!store.isAbsolute || !store.isFile) {
        throw GradleException("harw signing config incomplete: storeFile")
    }
    if (store.canonicalFile.toPath().startsWith(repoRoot.toPath())) {
        throw GradleException("harw signing config invalid: storeFile must be outside the repository")
    }
    store.canonicalFile
}
val signDebugWithStableKey = signingProps?.getProperty("signDebug")?.trim() != "false"

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
        versionCode = 18
        versionName = "0.3.15"
        ndk { abiFilters += "arm64-v8a" }
    }

    buildFeatures { compose = true }

    val harwStable = if (signingProps != null && signingStoreFile != null) {
        signingConfigs.create("harwStable") {
            storeFile = signingStoreFile
            storePassword = signingProps.getProperty("storePassword").trim()
            keyAlias = signingProps.getProperty("keyAlias").trim()
            keyPassword = signingProps.getProperty("keyPassword").trim()
        }
    } else {
        null
    }

    buildTypes {
        if (harwStable != null && signDebugWithStableKey) {
            debug { signingConfig = harwStable }
        }
        release {
            if (harwStable != null) signingConfig = harwStable
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
    implementation(platform("com.google.firebase:firebase-bom:35.2.0"))
    implementation("com.google.firebase:firebase-messaging")
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
