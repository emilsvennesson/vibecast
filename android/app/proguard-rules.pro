# JNA + UniFFI: keep native-facing classes/callbacks. (Minify is disabled in
# release for now; these keeps make enabling R8 later safe.)
-dontwarn java.awt.*
-keep class com.sun.jna.** { *; }
-keepclassmembers class * extends com.sun.jna.** { public *; }
-keep class * implements com.sun.jna.** { *; }

# Generated UniFFI bindings (JNA structures/callbacks resolved reflectively).
-keep class uniffi.** { *; }
