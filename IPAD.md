# CadCraft Studio on iPad and iPhone — the routine (on a Mac)

Track TB in getcadcraft.com's `docs/0.5-PART3.md` (TB.3); **Universal: one app for iPad and iPhone (owner, 2026-10-19: "universal is fine"; briefly iPad-only that day)**: `TARGETED_DEVICE_FAMILY "1,2"`; the iPhone layout has not been checked on a real iPhone yet (IPH.3 run sheet). Minimum iPadOS/iOS 15.0. Studio is the iPad and iPhone app: one install, the eight apps inside, the
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
  opened in place; iPad every orientation, iPhone upright and both sideways. The device family is the Xcode project's
  build setting (`gen/apple`, iPhone + iPad), never `UIDeviceFamily` in a plist.
- `gen/apple/cadcraft-desktop_iOS/PrivacyInfo.xcprivacy` is the privacy manifest App Store Connect wants (file timestamps of
  picked documents and the app's own files; elapsed time; no tracking, nothing collected). It is in the project's Resources. The prompt text is English for now (an `InfoPlist.strings` per language
  can follow).
- `src-tauri/build.rs` links `ios/plugin` (Swift: documents, Bonjour) when it builds for iOS; it copies Tauri's Swift API
  into `ios/.tauri/` first (git-ignored).

## No iPad or iPhone? The Simulator (owner, 2026-10-15; iPhone 0.6)

Xcode's Simulator runs Studio on the Mac itself: no device and no signing team needed (Apple silicon Mac). One simulator
build installs on any iPad or iPhone simulator; `--sim-device` only names the one in the printed run line.

```sh
rustup target add aarch64-apple-ios-sim
xcrun simctl list devices available | grep -E 'iPad|iPhone'   # e.g. "iPad Pro 13-inch (M4)", "iPhone 17 Pro"
node build.mjs studio --edition ../suite/cadcraft-studio-browser-vdev.zip --target ios --sim --sim-device "iPhone 17 Pro"
open -a Simulator
xcrun simctl boot "iPhone 17 Pro"                       # and/or an iPad: both can run at once
xcrun simctl install booted "out/CadCraft-Studio-dev-ios-simulator.app"
xcrun simctl launch booted com.getcadcraft.studio
```

If the build says no simulator .app was made, run it straight on the simulator instead (it prints the exact line):
`cargo tauri ios dev --config stage/studio/tauri.override.json "iPad Pro 13-inch (M4)"`.

Every build ends with a bundle check (`checkAppleBundle` in build.mjs, with `plutil`): iPhone + iPad, the orientations, the
local network keys and the privacy manifest must be in the built app, or the build fails.

The Simulator covers layout, the hub, the eight apps, files from the simulated Files app, sign-in and Live on the Mac's own
network. It can't show: Apple Pencil pressure, real touch feel, real performance, or Bonjour between two separate devices.

## Each build

```sh
node build.mjs studio --edition <suite folder or zip> --target ios             # a test build for your own iPad or iPhone → out/…-ios-test.ipa
node build.mjs studio --edition <suite folder or zip> --target ios --release   # the App Store Connect export → out/CadCraft-Studio-<v>.ipa
```

Try a test build on a device with Xcode (Window › Devices and Simulators › drag the .ipa), or `cargo tauri ios dev` with the
iPad or iPhone plugged in. The device must be registered in the DDG team's developer account for a development build. The `--release` build goes to TestFlight with Transporter or `xcrun altool`; store builds have no sign-in
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

## What to check on an iPhone (0.6 IPH.3, owner)

The iPhone run sheet is `docs/0.6-sessions/platforms-runsheets.md` § IPH.3 in the site repo. In short: the list above, plus
the phone layout (panels as sheets from the bar at the bottom, one-row toolbars), the notch / Dynamic Island and the home
indicator not covering the app bar or the bottom bar (upright and sideways), the keyboard over the command line, and a fold:
the outer and inner screens when Apple ships the folding iPhone or its simulator profile.

## Before the App Store (owner)

- **Export compliance**: App Store Connect asks whether the app uses encryption. Studio uses the system's HTTPS and Studio
  Live's own end-to-end encryption (SPAKE2 + ChaCha20-Poly1305, standard algorithms): answer as your counsel advises;
  `ITSAppUsesNonExemptEncryption` is left out of Info.ios.plist on purpose until then.
- Privacy labels: no tracking; the sign-in (test builds) sends the device's model ("iPad") as its name.
- Screenshots: the store kit (`stores/`) takes them (TB.6): iPad 13-inch `ipad13-*` and iPhone 6.9-inch `iphone69-*` in
  `stores/listings/studio/screenshots/`; App Store Connect scales the 6.9-inch set for smaller iPhones.

## Not yet

- Opening a file from the Files app's Share sheet or "Open in" (document types / `CFBundleDocumentTypes`): Studio opens
  files from its own picker today.
- Studio Live hosting in the background: iPadOS suspends Studio when it leaves the screen, so a host must stay on screen.
- iCloud files not downloaded yet: the first open may fail until the Files app has fetched them.
