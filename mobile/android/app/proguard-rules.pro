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
