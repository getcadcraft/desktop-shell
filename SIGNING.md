# Signing the Windows installer (W.6; code signing for every platform comes in 0.6)

Until this is done, SmartScreen says "Windows protected your PC" and the publisher shows as unknown; /download says so
in its notices. Signing is wired; only the certificate is missing (the owner's).

## What the owner provides (decision 9)

One of:

- **Azure Trusted Signing** (Microsoft's signing service): an Azure subscription, a Trusted Signing account, identity
  validation for The Data Design Group, Inc., and a certificate profile. Then an app registration with the "Trusted
  Signing Certificate Profile Signer" role: `AZURE_TENANT_ID`, `AZURE_CLIENT_ID`, `AZURE_CLIENT_SECRET`, plus the account's
  endpoint, account name and profile name.
- **An OV or EV code-signing certificate** from a public CA. Since 2023 its key lives in hardware (a USB token or a cloud
  HSM), so signing happens where that key is: the owner's Windows machine, or the CA's cloud signing service.

Either way the chain must reach the Microsoft Trusted Root Program, which the Microsoft Store also requires.

## How a build signs

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

## Checking a signed build

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
