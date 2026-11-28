# Preserve class names for reflection, release checks, and readable stack traces.
-dontobfuscate
# Retain source locations in crash reports.
-keepattributes SourceFile,LineNumberTable
# JNA is accessed dynamically by UniFFI and native code.
-keep class com.sun.jna.** { *; }
# JNA implementations and subclasses are accessed dynamically.
-keep class * extends com.sun.jna.** { *; }
# JNA interfaces can be implemented by dynamically accessed classes.
-keep class * implements com.sun.jna.** { *; }
# UniFFI bindings are accessed by native code.
-keep class uniffi.** { *; }
# Gomobile runtime and bridge receive JNI callbacks by name.
-keep class go.** { *; }
# The application bridge receives gomobile JNI callbacks by name.
-keep class dev.peppy.gmlink.** { *; }
# Restricted setup loads transport and cryptographic providers reflectively.
-keep class io.github.muntashirakon.** { *; }
# Restricted setup loads Conscrypt reflectively.
-keep class org.conscrypt.** { *; }
# Restricted setup loads Bouncy Castle reflectively.
-keep class org.bouncycastle.** { *; }
# JNA's desktop AWT integration is unavailable on Android.
-dontwarn java.awt.Component
# JNA's desktop AWT integration is unavailable on Android.
-dontwarn java.awt.GraphicsEnvironment
# JNA's desktop AWT integration is unavailable on Android.
-dontwarn java.awt.HeadlessException
# JNA's desktop AWT integration is unavailable on Android.
-dontwarn java.awt.Window
