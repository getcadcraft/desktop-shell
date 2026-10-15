# android/ — what the Android build of CadCraft Studio carries besides the generated project

The Android project itself is `src-tauri/gen/android` (made by `cargo tauri android init`, then edited and committed:
the shell's own plugin `app/src/main/java/com/getcadcraft/studio/CadcraftPlugin.kt`, the manifest, ProGuard rules).
Track TB in getcadcraft.com's `docs/0.5-PART3.md`.

## maven/ — vendored, never fetched while building

| Artifact | Version | From | SHA-1 | License |
|---|---|---|---|---|
| `org.rustls:rustls-platform-verifier` (AAR: one small class jar + a network security config for CRL fetches) | 0.2.0 | the rustls-platform-verifier project's own Maven archive, `https://raw.githubusercontent.com/rustls/rustls-platform-verifier/maven-archive/android-release-support/maven/` | `28b6b316c2749a7c94b4d19061f12c64759a0e27` (matches the archive's `.sha1`) | MIT OR Apache-2.0 |

It is the Kotlin half of the `rustls-platform-verifier` crate: Rust's TLS (rustls) asks Android's own trust manager whether
a server's certificate chain is good, so the shell ships no CA list. Its version must equal `rustls-platform-verifier-android`
in `src-tauri/Cargo.lock` (`build.mjs --target android` checks). To move to a new version: fetch the `.aar`, `.pom` and their
`.sha1` from the address above into `maven/org/rustls/rustls-platform-verifier/<version>/`, check the SHA-1, update the
version in `gen/android/app/build.gradle.kts` and this table.
