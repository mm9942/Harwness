# JNI and JNA resolve native classes, members, and Structure fields by name.
-keep class com.sun.jna.** { *; }
-keep class uniffi.harw_mobile_ffi.** { *; }

# SavedStateViewModelFactory reflects on this constructor.
-keepclassmembers class dev.harw.mobile.HarwWindowModel {
    public <init>(androidx.lifecycle.SavedStateHandle);
}

# JNA's optional desktop-only AWT helpers are kept with the native bindings.
# Harw does not call these APIs and Android does not provide AWT.
-dontwarn java.awt.Component
-dontwarn java.awt.GraphicsEnvironment
-dontwarn java.awt.HeadlessException
-dontwarn java.awt.Window

# Opted-in vault JNI resolves these exact class/method names. Keeping names does not enable it.
-keep class dev.harw.mobile.AndroidUiaRoomPinNative {
    private native long provisionNewNative(android.app.Application, long, long, long, java.nio.ByteBuffer);
    private native long openExistingNative(android.app.Application, long, long, long, java.nio.ByteBuffer);
    private native long resumeEnrollmentNative(android.app.Application, long, long, long, java.nio.ByteBuffer);
    private native int canRestartUncommittedNative(android.app.Application, long, long, long);
    private native int retireUncommittedNative(android.app.Application, long, long, long);
    private native long openExistingBiometricNative(android.app.Application, long, long, long, java.nio.ByteBuffer);
    private native int revokeNative(long, long, long, long);
    private native long libraryNative(android.app.Application, long, long, long, long, int, int, java.nio.ByteBuffer);
    private native long cloudWorkNative(android.app.Application, long, long, long, long, java.lang.String, java.lang.String, int, java.nio.ByteBuffer);
    private native long memoryNative(long, long, long, long, long, long, int, int, java.nio.ByteBuffer);
    private native long readRoomNative(long, long, long, long, java.nio.ByteBuffer);
    private native long reserveRoomNative(long, long, long, long, long);
    private native long cancelRoomNative(long, long, long, long, long);
    private native int parkRoomVersionNative();
    private native int hasPendingParkedResponseNative(long, long, long);
    private native long parkRoomNative(long, long, long, long, long);
    private native long cancelParkedRoomNative(long, long, long, long, long);
    private native long runRoomNative(android.app.Application, long, long, long, long, long, long, java.nio.ByteBuffer);
}
-keep class dev.harw.mobile.AndroidVaultPinKdf {
    private native int deriveInPlace(java.nio.ByteBuffer);
}
# Native application provenance check uses this JVM name; manifest also identifies it.
-keep,allowoptimization class dev.harw.mobile.HarwApplication

# Existing capability JNI name, kept unchanged for the opted-in release ABI.
-keep class dev.harw.mobile.LocalUiaDevCapability {
    private native java.lang.String activeProfileNative(android.app.Application);
}
