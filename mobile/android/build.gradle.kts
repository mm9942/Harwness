buildscript {
    repositories { mavenCentral() }
    dependencies { classpath("org.jetbrains.kotlin:kotlin-gradle-plugin:2.4.21") }
}

plugins {
    id("com.android.application") version "9.5.2" apply false
    id("org.jetbrains.kotlin.plugin.compose") version "2.4.21" apply false
    id("com.google.gms.google-services") version "4.6.0" apply false
}
