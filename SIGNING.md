# Code signing on every platform (W.6; 0.6 SIGN; 0.7 SIGN)

| Platform | Signed with | Script | State (2026-10-22) | Dry run |
|---|---|---|---|---|
| Windows | Azure Artifact Signing (DDG), jsign | `build.mjs` → `CC_SIGN_COMMAND='sign-azure.sh %1'` | sign-in works (`--check`); the signing service still refuses the profile (403): Microsoft's identity validation is not finished | `node sign-dryrun.mjs windows --edition <suite>` |
| macOS | Developer ID Application (DDG team KXB5X8ZLX7) + notarization | `build.mjs --target macos` with `APPLE_*` (Tauri) | waits on the owner's certificate | on the Mac, below |
| iPad / iPhone | Apple Distribution, automatic signing (DDG team) | `build.mjs --target ios [--release]` | waits on the owner's App Store record | on the Mac, below |
| Android | our release key (`secrets/android`, 2026-10-15) | `build.mjs --target android --release` / `--apk` | **signing now** (the published APK) | `node sign-dryrun.mjs android` |
| Linux | an OpenPGP key over `SHA256SUMS` | `stores/linux/sign-sums.sh <folder>` | waits on the owner's release key | `node sign-dryrun.mjs linux [folder]` |
| Microsoft Store (MSIX) | **the Store signs it** (Microsoft's certificate); only the package identity is ours | `build.mjs studio --store --msix` (packed here by `msix/pack.mjs`, or `makeappx` on Windows) | packs here; WACK needs the Windows SDK; the name reservation is the owner's | `node test/msix.test.mjs` |
| Website MSIX | the Azure certificate (`CC_MSIX_SIGN_COMMAND`); publish refuses anything else | `build.mjs studio --msix` | built, not published: waits on Azure | `getcadcraft.com bin/test-update-feed.php` |
| Updates (every platform) | the owner's minisign key (Tauri updater) | `build.mjs` / `stores/linux/build.mjs` with `secrets/updater/credentials.env`, `updater-feed.mjs`, the site's `UpdateFeed` | **key made 2026-10-24** (`secrets/updater/`); the owner backs it up offline | `node test/feed.test.mjs`, `getcadcraft.com bin/test-update-feed.php` |

**`node sign-preflight.mjs`** says, per platform, which of these values are missing from the environment a build would see
(names only, never values). Run it before a release build; it exits 1 while anything is missing.

`node sign-dryrun.mjs verify <file.exe>` says who signed a Windows file (from its Authenticode table) — for the real build too.

## Updates: the key, the one endpoint, the entries (0.7: D7.54, SIGN, MSIX; owner 2026-10-24)

**The key exists** (made 2026-10-24 with `cargo tauri signer generate`, tauri-cli 2.12.2, password-protected, key id
`E0AB66CFCD391317`). Where it lives:

| What | Where | Who reads it |
|---|---|---|
| private key | `/var/www/CADCRAFT/secrets/updater/cadcraft-updater.key` (root, 600; dir 700) | builds, through `TAURI_SIGNING_PRIVATE_KEY` |
| its password | `secrets/updater/credentials.env` `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` (600) | the same |
| public key | `secrets/updater/credentials.env` `CC_UPDATE_PUBKEY`, `desktop-shell/updater-key.pub` (public; in this repo), and the site's `.env` `CC_UPDATE_PUBKEY` | builds (into `shell.json`), the site (checks every entry when it publishes) |

Never print, copy or commit the private key or the password. **Back both up offline** (the owner): losing either means no
installed copy can ever be updated in place again. A build picks the key up the way the Android credentials are picked up:

```
set -a; . /var/www/CADCRAFT/secrets/updater/credentials.env; set +a     # TAURI_SIGNING_PRIVATE_KEY(_PASSWORD), CC_UPDATE_PUBKEY
node build.mjs studio --edition <suite> --webview2-fixed <runtime> --portable          # website .exe + portable zip
cd ../stores && node linux/build.mjs studio --edition <suite>                          # .deb + AppImage
```

**One endpoint for every edition** (owner 2026-10-24). Every build with the key gets in `shell.json`
`updates {endpoint, pubkey, auto, edition}`, endpoint
`https://getcadcraft.com/api/v1/updates/{{target}}/{{arch}}/{{current_version}}?app=studio&edition={edition}` (Tauri fills
`{{…}}`, the shell fills `{edition}`; `CC_UPDATE_ENDPOINT` overrides it, https only — http only in an `--update-test` build).
The site's `UpdateFeed` service answers **204** (nothing newer for this copy) or Tauri's `{version, notes, pub_date, url,
signature}`, from published files only, after the update rules (`getcadcraft.com/public_html/core/data/update-rules.json`:
hold a version back, cap at a version, a stepping stone, off for a range, rollout off — the site CLAUDE.md § Updates).

| Edition (`shell.json`) | Build | What "Check for updates" does |
|---|---|---|
| `installer` | `build.mjs` (NSIS, fixed WebView2 runtime) | Tauri updater: download, check the signature, run the new installer, restart |
| `portable` | `build.mjs --portable` | asks the endpoint and **only tells** ("download it", opens /download): a running portable copy can't safely swap its own program, runtime and app folder |
| `deb`, `appimage` | `stores/linux/build.mjs` (one `shell.json`, edition `linux`: the shell sends `appimage` when it runs as one) | Tauri updater (AppImage replaced; `.deb` through `pkexec dpkg -i`) |
| `dmg` | `build.mjs --target macos` | entries are recorded, but /download does not publish the `.app.tar.gz` yet: the Mac gets 204 until it does |
| `msix-web` | `build.mjs --msix` (website package) | App Installer: `Package.CheckUpdateAvailabilityAsync`, then `PackageManager.AddPackageByAppInstallerFileAsync` (src-tauri/src/package.rs) — the `.appinstaller` comes from the same service |
| `msix-store` | `build.mjs --store --msix` | the Store: `StoreContext` (`GetAppAndOptionalStorePackageUpdatesAsync` → `RequestDownloadAndInstallStorePackageUpdatesAsync`); the endpoint always answers 204 |

The Tauri updater never runs inside a package (package identity → Windows' route; a package installed from a file answers
`not_store`). **The fixed-runtime installer carries the updater since 0.7:** the old rule ("Tauri forces the download
bootstrapper") was true only of the v1 updater (`createUpdaterArtifacts: "v1Compatible"` rebuilds the installer with
`DownloadBootstrapper`); the v2 artifacts (`createUpdaterArtifacts: true`) sign the very installer that was built, fixed
runtime and all (tauri-bundler 2.10 `bundle.rs` / `updater_bundle.rs`).

**Entries, not a latest.json.** Each build signs every updatable file (Tauri for the installer; `tauri signer sign` for the
portable zip and, after `fixDeb`, the `.deb`) and writes one entry per file: `out/updates/studio-<x.y.z>/<edition>-<target>-<arch>.json`
{version, edition, target, arch, file (its /download name), sha256, signature} (`updater-feed.mjs add`). `Downloads::publish()`
reads the entries of the version it publishes (desktop-shell/out and stores/out/linux), keeps the ones whose file it published
with the same SHA-256, **checks each signature against `CC_UPDATE_PUBKEY`** (minisign, in PHP: `UpdateFeed::verifySignature`)
and writes `downloads/<x.y>/updates.json`; a bad entry refuses the whole set. Gate before publishing:

```
node updater-feed.mjs check studio 0.7.0 --out out,../stores/out/linux      # installer, portable, deb, appimage each have an entry
```

**Automatic checks are on by default and run once a day** (decision 10, owner 2026-10-24): at most one automatic check per 24 h
while Studio runs, timed from the last check in the app data folder's `updates.json` (a start a day later checks ~30 s in; a Studio
left running checks again when a day has passed). The person's own choice (off) always wins; `CC_UPDATE_AUTO=0` builds with them off.
The website MSIX does the same through App Installer's own check; the Store package follows the Store. Nothing installs without
the person saying yes.
Settings › Updates in Studio shows the version and edition, where updates come from, the last check (kept in the app data
folder's `updates.json`), the latest version for this edition with its notes and Install, and the automatic-checks switch.

**Test builds** (`build.mjs --update-test`, `CC_UPDATE_ENDPOINT=http://127.0.0.1:<port>/…`): their own name ("CadCraft Studio
Update Test") and identifier (`com.getcadcraft.studio.updatetest`), no install hooks, the plugin's insecure-transport switch
on, outputs in `out/update-test/` (never where Downloads looks). The test server is the site's real service on a fixture set:
`getcadcraft.com/tools/update-feed-test/{router,publish}.php`. Never publish a test build.

### The website MSIX (built, not published)

`build.mjs studio --edition <suite> --msix` (no `--webview2-fixed`: a package uses the system's WebView2) → the website
package `out/CadCraft Studio-<v>.msix` + `out/CadCraft Studio-<v>.appinstaller`, packed on Linux by `msix/pack.mjs`. Identity:
`MSIX_WEB_PUBLISHER` = the Azure certificate's subject **exactly** (Windows refuses a package whose Publisher is not its
signer's subject), `MSIX_WEB_IDENTITY_NAME` (default `CadCraft.Studio`). Signed by `CC_MSIX_SIGN_COMMAND` (`%1` = the file),
e.g. jsign with Azure Artifact Signing once the profile works:
`CC_MSIX_SIGN_COMMAND='java -jar /opt/jsign/current/jsign-7.5.jar --storetype TRUSTEDSIGNING --keystore eus.codesigning.azure.net --storepass <token> --alias cadcraft-signing/cadcraft-public --tsaurl http://timestamp.acs.microsoft.com --tsmode RFC3161 %1'`.
**Never publish an unsigned or self-signed package:** `Downloads::publish()` holds the MSIX back unless its signature is
real (`UpdateFeed::msixInfo`: signer = the manifest's Publisher, not self-signed, code signing, a chain to a certificate in the
PEM bundle `.env MSIX_TRUST_BUNDLE` — the owner saves the Azure chain's root there once). People install through
`/api/v1/updates/studio.appinstaller`, which App Installer re-reads to update.

## Windows

Until this is done, SmartScreen says "Windows protected your PC" and the publisher shows as unknown; /download says so
in its notices. Signing is wired; only the certificate is missing (the owner's).

### What the owner provides (decision 9)

One of:

- **Azure Trusted Signing** (Microsoft's signing service): an Azure subscription, a Trusted Signing account, identity
  validation for The Data Design Group, Inc., and a certificate profile. Then an app registration with the "Trusted
  Signing Certificate Profile Signer" role: `AZURE_TENANT_ID`, `AZURE_CLIENT_ID`, `AZURE_CLIENT_SECRET`, plus the account's
  endpoint, account name and profile name.
- **An OV or EV code-signing certificate** from a public CA. Since 2023 its key lives in hardware (a USB token or a cloud
  HSM), so signing happens where that key is: the owner's Windows machine, or the CA's cloud signing service.

Either way the chain must reach the Microsoft Trusted Root Program, which the Microsoft Store also requires.

### How a build signs

`build.mjs` hands `CC_SIGN_COMMAND` to Tauri's `signCommand`, which runs it for every file it signs: the app's own
`CadCraft Studio.exe`, the uninstaller and the installer. `%1` is the file. The WebView2 runtime inside Studio is already
signed by Microsoft and is left as it is.

- **In CI** (`.github/workflows/windows.yml`, on Windows): set the secrets above; the job uses `trusted-signing-cli` and
  prints each file's Authenticode status. MSI too.
- **Cross-built here on Linux**: the same variable with a signer that runs on Linux, e.g. jsign (Apache-2.0, Java) for
  Trusted Signing:
  ```
  CC_SIGN_COMMAND='jsign --storetype TRUSTEDSIGNING --keystore <endpoint> --storepass "$(az account get-access-token --resource https://codesigning.azure.net --query accessToken -o tsv)" --alias <account>/<profile> --tsaurl http://timestamp.acs.microsoft.com --tsmode RFC3161 %1' \
    node build.mjs studio --edition … --webview2-fixed … --version …
  ```
  **Route chosen (owner, 2026-10-15): Azure Artifact Signing**, Basic plan; account `cadcraft-signing` (East US,
  `eus.codesigning.azure.net`), certificate profile `cadcraft-public` (created once Microsoft completes the identity
  validation for The Data Design Group, Inc.), app registration `cadcraft-build-signer` with the Certificate Profile
  Signer role; its secret expires 2027-10-14 (renew before then). `sign-azure.sh` here does the signing with jsign 7.5
  (no Azure CLI): `sign-azure.sh --check` proves the sign-in, `CC_SIGN_COMMAND='…/sign-azure.sh %1'` signs a build. The
  values come from the environment (the site's `.env` outside the repos holds them; only the owner writes the secret).

### The dry run (0.6)

`node sign-dryrun.mjs windows --edition ../studio/out/studio-suite/v0.5` builds Studio (installer version 0.0.99, no WebView2
inside) with `CC_SIGN_COMMAND` = `sign-azure.sh` in its dry mode (`SIGN_DRY_KEYSTORE`: a throwaway self-made certificate
through the very same jsign call, no Azure, no timestamp) and checks that Tauri called the signer for the app, the
uninstaller and the installer, and that the installer carries the signature. The build lands in `stage/sign-dryrun/`, never
`out/`. The first real signed build is the same command line with the dry variables left out:
```
set -a; eval "$(grep -E '^(AZURE_[A-Z_]+|JSIGN_JAR)=' /var/www/CADCRAFT/.env)"; set +a   # only these values from .env
./sign-azure.sh --check                        # sign-in
CC_SIGN_COMMAND="$PWD/sign-azure.sh %1" node build.mjs studio --edition <suite> --webview2-fixed <runtime dir> --version <x.y.z>
node sign-dryrun.mjs verify "out/CadCraft Studio-<x.y.z>-setup.exe"   # subject The Data Design Group, Inc., timestamped
```
When the profile is ready, `sign-azure.sh` stops getting 403 from `eus.codesigning.azure.net`; nothing else changes.

### Checking a signed build

- Here: `Downloads::peSigned()` (site) sees the Authenticode table; `bin/publish-downloads.php` then records
  `signed: true` in the manifest, and /download stops saying "Not code-signed" for that file.
- On Windows: `Get-AuthenticodeSignature .\CadCraft-Studio-<v>-setup.exe` → Status `Valid`, signer The Data Design
  Group, Inc.; the installer's own UAC/SmartScreen prompt names the publisher (WINDOWS-CHECK.md step 2).

## macOS test build (unsigned, no Apple account)

For the owner's own Mac checks before the Apple Developer account exists. Don't publish a test build. The released
CadCraft Studio 0.5 for macOS is also unsigned and not notarized (signing for every platform comes in 0.6), built on the
owner's Mac from the released suite; its release text says so. Gatekeeper asks on first open (right-click the app › Open;
on macOS 15 and later System Settings › Privacy & Security › Open Anyway; or `xattr -cr "/Applications/CadCraft Studio.app"`).

1. The repo `getcadcraft/desktop-shell` exists on GitHub with this tree on `main` (Actions enabled).
2. Pack the dev suite: `CADCRAFT_SITE=<site> node <studio>/pack/suite.mjs --zip` → `studio/out/cadcraft-studio-browser-vdev.zip`;
   `sha256sum cadcraft-studio-browser-vdev.zip > SHA256SUMS`.
3. A pre-release named `suite-dev` on this repo with those two files:
   `gh release create suite-dev --repo getcadcraft/desktop-shell --prerelease --title "Dev suite for test builds" <zip> SHA256SUMS`
   (replace the files with `gh release upload suite-dev … --clobber` after each repack).
4. Actions › macOS Studio › Run workflow, version `dev`. The artifact `studio-dev-macos` holds a universal `.dmg`
   (Apple silicon + Intel) and its SHA256SUMS. The run warns "unsigned" and "Gatekeeper refuses it": expected.
5. On the Mac: the macOS part of WINDOWS-CHECK.md's Studio list, plus local-network permission when Studio hosts (Studio Live).

## macOS (0.6 SIGN; the owner's Apple Developer account, DDG team KXB5X8ZLX7)

What the owner makes once, in the DDG team's account (developer.apple.com › Certificates): a **Developer ID Application**
certificate (export it with its key as a .p12 for the Mac that builds), and for notarization an **App Store Connect API key**
(Users and Access › Integrations › Team key, role Developer: the .p8 file, its Key ID and the Issuer ID). An Apple ID with an
app-specific password works too but the API key is preferred (no person's password).

Tauri signs and notarizes when these are set (build.mjs passes them through; Tauri turns on the hardened runtime):
```
APPLE_SIGNING_IDENTITY="Developer ID Application: <the name `security find-identity -v -p codesigning` shows> (KXB5X8ZLX7)"
APPLE_TEAM_ID=KXB5X8ZLX7
APPLE_API_ISSUER=<issuer id>  APPLE_API_KEY=<key id>  APPLE_API_KEY_PATH=<path to AuthKey_<key id>.p8>
# on CI only (no login keychain): APPLE_CERTIFICATE=<base64 of the .p12>  APPLE_CERTIFICATE_PASSWORD=<its password>
node build.mjs studio --edition <suite> --target macos --version <x.y.z>
```
After the build, build.mjs checks with Apple's own tools and fails otherwise: `codesign --verify --deep --strict`, signed
by a Developer ID Application certificate of team `APPLE_TEAM_ID`, hardened runtime, `xcrun stapler validate` and
`spctl --assess` = "Notarized Developer ID". Without notarization variables it warns "never publish it".

**Dry run on the Mac** (no certificate yet): the same command without the variables builds an ad hoc signed .dmg (what
0.5 shipped) and passes `codesign --verify`. With the certificate but before the API key exists, it signs and warns that it
is not notarized. Run sheet: `docs/0.6-sessions/platforms-runsheets.md` § macOS signing (site repo).

The Mac App Store build (`--store`) is signed by Xcode/Transporter with an Apple Distribution certificate and the
`macos/Studio.MAS.entitlements` sandbox; it is not notarized (the store does that).

## iPad and iPhone

Automatic signing with the DDG team (`tauri.ios.conf.json` `developmentTeam`, the Xcode project's `DEVELOPMENT_TEAM`):
Xcode makes and renews the certificates and profiles while it is signed in to an Apple ID of the DDG team (Xcode ›
Settings › Accounts). `build.mjs --target ios` = a development build for registered devices; `--release` = the App Store
Connect export (`--export-method app-store-connect`), uploaded with Transporter. Needs the app record
`com.getcadcraft.studio` in App Store Connect first. `test/apple.test.mjs` checks the project (team, device family,
privacy manifest); build.mjs checks the built bundle.

## Android

The release key made 2026-10-15 (`/var/www/CADCRAFT/secrets/android/`: keystore + `credentials.env`; never printed, copied
or committed; the owner keeps an offline backup). `set -a; . secrets/android/credentials.env; set +a`, then
`node build.mjs studio --edition <suite> --target android --release` → a signed AAB for Google Play, or `--apk` → the
website's APK. Google Play: choose **"Use my own key"** for app signing (upload this key's certificate with Play's PEPK
tool), so the website APK and the Play install are the same app and update each other; with Google's own app signing key
they can't. Run sheet: § Google Play.

## Linux

`stores/linux/sign-sums.sh <folder>` signs the folder's `SHA256SUMS` (checked against the files first) with the release
OpenPGP key `CC_GPG_KEY` → `SHA256SUMS.asc`. One signature covers the .deb and the AppImage. The owner makes the key once
(`gpg --quick-gen-key "CadCraft releases <releases@getcadcraft.com>" ed25519 sign 2y`), keeps it in `secrets/`, and
publishes the public key on the site (owner's page); people check with `gpg --verify SHA256SUMS.asc SHA256SUMS && sha256sum
-c --ignore-missing SHA256SUMS`. Flathub builds and signs its own packages.
