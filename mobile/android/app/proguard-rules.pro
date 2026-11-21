# JNI and JNA resolve native classes, members, and Structure fields by name.
-keep class com.sun.jna.** { *; }
-keep class uniffi.harw_mobile_ffi.** { *; }

# SavedStateViewModelFactory reflects on this constructor.
-keepclassmembers class dev.harw.mobile.HarwWindowModel {
    public <init>(androidx.lifecycle.SavedStateHandle);
}
