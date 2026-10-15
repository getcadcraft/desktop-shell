# CadCraft Studio on iPad — the routine (on a Mac)

Track TB in getcadcraft.com's `docs/0.5-PART3.md` (TB.3). Studio is the iPad app: one install, the eight apps inside, the
same shell as the desktop and Android (Tauri 2). **Everything here was prepared on Linux and has not been built yet**: the
first run on the Mac is the real check. Android builds on the CadCraft server (`build.mjs --target android`, see README).

## What you need (owner)

- A Mac with Xcode (current, with the iPadOS SDK) and its command-line tools; signed in with the Apple Developer account
  (decision 12: whose account and team).
- Rust (`rustup`), then `rustup target add aarch64-apple-ios aarch64-apple-ios-sim`, and `cargo install tauri-cli --version
  "^2" --locked`. Node 20+, Python 3 + Pillow (the icon).
- This repo, and Studio's suite folder (the studio repo's `pack/suite.mjs` output, or its release zip), copied to the Mac.

## Once: make the Xcode project and commit it

```sh
cd desktop-shell
cargo tauri ios init            # writes src-tauri/gen/apple (the Xcode project), from tauri.conf.json + tauri.ios.conf.json
git add src-tauri/gen/apple     # committed, like gen/android
```

Then in Xcode (`open src-tauri/gen/apple/*.xcodeproj`), once: **Signing & Capabilities** → your team; bundle id
`com.getcadcraft.studio`. Nothing else needs adding by hand:

- `src-tauri/Info.ios.plist` is merged into the app's Info.plist by Tauri: local network prompt text
  (`NSLocalNetworkUsageDescription`), Bonjour `_ccstudio._tcp` (`NSBonjourServices`, Studio Live; LV.13's iPad keys), files
  opened in place, iPad only, every orientation. The prompt text is English for now (an `InfoPlist.strings` per language
  can follow).
- `src-tauri/build.rs` links `ios/plugin` (Swift: documents, Bonjour) when it builds for iOS; it copies Tauri's Swift API
  into `ios/.tauri/` first (git-ignored).

## No iPad? The iPad Simulator (owner, 2026-10-15)

Xcode's iPad Simulator runs Studio on the Mac itself: no iPad and no signing team needed (Apple silicon Mac).

```sh
rustup target add aarch64-apple-ios-sim
xcrun simctl list devices available | grep iPad          # pick one, e.g. "iPad Pro 13-inch (M4)"
node build.mjs studio --edition ../suite/cadcraft-studio-browser-vdev.zip --target ios --sim
open -a Simulator
xcrun simctl boot "iPad Pro 13-inch (M4)"
xcrun simctl install booted "out/CadCraft-Studio-dev-ipad-simulator.app"
xcrun simctl launch booted com.getcadcraft.studio
```

If the build says no simulator .app was made, run it straight on the simulator instead (it prints the exact line):
`cargo tauri ios dev --config stage/studio/tauri.override.json "iPad Pro 13-inch (M4)"`.

The Simulator covers layout, the hub, the eight apps, files from the simulated Files app, sign-in and Live on the Mac's own
network. It can't show: Apple Pencil pressure, real touch feel, real performance, or Bonjour between two separate devices.

## Each build

```sh
node build.mjs studio --edition <suite folder or zip> --target ios             # a test build for your own iPad → out/…-ipad-test.ipa
node build.mjs studio --edition <suite folder or zip> --target ios --release   # the App Store Connect export → out/CadCraft-Studio-<v>.ipa
```

Try a test build on a device with Xcode (Window › Devices and Simulators › drag the .ipa), or `cargo tauri ios dev` with the
iPad plugged in. The `--release` build goes to TestFlight with Transporter or `xcrun altool`; store builds have no sign-in
(decision 11 still open; the same rule as the desktop's store builds).

## What to check on the iPad (the TB.1–TB.5 list, as on Android)

1. Studio opens on the hub; the bar says **Tablet**. Open each app: it fills the screen; the app bar's Studio button and
   the hub's **Open apps** list switch between them; **Close** asks when an app has unsaved work.
2. **Files:** Open a file… (the Files app's picker; iCloud Drive, On My iPad, a USB drive): the drawing opens and its name
   shows; save writes it back in place; quit Studio, open it again: the file is in Recent files and reopens. A DWG from
   the Files app's Share › Studio is not wired yet (see "Not yet").
3. **Turned upright:** the side panels become sheets (the two panel buttons in the app bar); the drawing gets the width.
   Sideways: they dock back.
4. **Keyboard:** the command line stays above the on-screen keyboard; Apple Pencil draws like a mouse; two fingers pinch
   and pan.
5. **Studio Live:** Join a live session with a code from a computer on the same Wi-Fi (iPadOS asks once for local network
   access: allow); join with an invite link; host from the iPad (the panel says to keep Studio on screen) and join it from
   a computer.
6. **Sign-in** (test build only): Settings › account → the code flow; the token is in the Keychain; sign out removes it.
7. PartCraft's Python and threads: WKWebView needs cross-origin isolation for SharedArrayBuffer; say what works.

Write what you find in `docs/0.5-sessions/tablets.md` § Inbox (or tell the session), with screenshots for the listings.

## Before the App Store (owner)

- **Export compliance**: App Store Connect asks whether the app uses encryption. Studio uses the system's HTTPS and Studio
  Live's own end-to-end encryption (SPAKE2 + ChaCha20-Poly1305, standard algorithms): answer as your counsel advises;
  `ITSAppUsesNonExemptEncryption` is left out of Info.ios.plist on purpose until then.
- Privacy labels: no tracking; the sign-in (test builds) sends the device's model ("iPad") as its name.
- Screenshots: the store kit (`stores/`) takes them from these builds (TB.6).

## Not yet

- Opening a file from the Files app's Share sheet or "Open in" (document types / `CFBundleDocumentTypes`): Studio opens
  files from its own picker today.
- Studio Live hosting in the background: iPadOS suspends Studio when it leaves the screen, so a host must stay on screen.
- iCloud files not downloaded yet: the first open may fail until the Files app has fetched them.
