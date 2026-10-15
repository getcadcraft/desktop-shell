# SPDX-License-Identifier: Apache-2.0
# CadCraft Studio (desktop-shell, TB.1/TB.2): classes reached by name or reflection.
# rustls-platform-verifier's Kotlin half is called from Rust through JNI.
-keep, includedescriptorclasses class org.rustls.platformverifier.** { *; }
# The shell's own plugin (src-tauri/src/mobile.rs registers it by name) and its argument classes (read by Jackson).
-keep class com.getcadcraft.studio.CadcraftPlugin { *; }
-keep class com.getcadcraft.studio.OpenArgs { *; }
-keep class com.getcadcraft.studio.CreateArgs { *; }
-keep class com.getcadcraft.studio.UriArgs { *; }
-keep class com.getcadcraft.studio.TreeArgs { *; }
-keep class com.getcadcraft.studio.SecretArgs { *; }
