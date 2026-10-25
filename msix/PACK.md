<!-- SPDX-License-Identifier: Apache-2.0 -->
# Packing CadCraft Studio as MSIX (0.7)

Two packages (owner, 2026-10-24: "MSIX through the Microsoft Store AND as a website download"):

| Package | Build | Identity | Signed by | Updated by |
|---|---|---|---|---|
| **Store** | `node build.mjs studio --edition <suite> --store --msix` → `out/CadCraft Studio-<v>-store.msix` | Partner Center's (`MSIX_IDENTITY_NAME`, `MSIX_PUBLISHER`, `MSIX_PUBLISHER_DISPLAY`) | the Store (upload it unsigned) | the Store (`StoreContext` in Studio's Check for updates) |
| **Website** | `node build.mjs studio --edition <suite> --msix` → `out/CadCraft Studio-<v>.msix` + `.appinstaller` | ours (`MSIX_WEB_IDENTITY_NAME` default `CadCraft.Studio`, `MSIX_WEB_PUBLISHER` = the Azure certificate's subject) | us, `CC_MSIX_SIGN_COMMAND` (Azure; **never published unsigned or self-signed** — the site refuses) | App Installer, from `getcadcraft.com/api/v1/updates/studio.appinstaller` |

Both are packed on Linux by **`msix/pack.mjs`** (an OPC zip with a block map: every file deflated block by block, its SHA-256 per
64 KiB, `[Content_Types].xml`; `node msix/pack.mjs check <file>` reads it back). Windows installed and updated packages made this way on
workstation11a (2026-10-25, docs/results/0.7/2026-10-24-msix-updates.md). It always writes the zip64 end records (Windows' signature
digest covers them: without them a signed package fails with HashMismatch). `makeappx` on Windows still works on the same layout.
The website package's shell.json has `updates.edition: msix-web`; the Store's `msix-store` and never an endpoint.

## What is built where

| Step | Where | Command |
|---|---|---|
| The program, the app folders, the manifest and the tiles (the **layout**) | Linux (cross-build) or Windows | `node build.mjs studio --edition <suite> --store --msix` |
| The layout's own check (every file the manifest names exists; store build; no update feed) | the same command | runs by itself; `node test/msix.test.mjs` for the generator |
| pack → `.msix` | here (`msix/pack.mjs`, the same command) or Windows (`makeappx`, below) | |
| Windows App Certification Kit (WACK) | **Windows** | below |
| Sideload install check on a clean Windows 10 / 11 | **Windows** | `WINDOWS-CHECK.md` § MSIX |
| Upload and Partner Center's own validation | the owner | Partner Center › Packages |

The certification kit (WACK) is a Windows SDK tool: nothing here has been through it. The layout is checked as far as
a Linux machine can check it (well-formed manifest read by a real XML parser, every referenced file present, tiles at the
declared sizes, the file types / link / firewall rules / one tile per app declared); the first Windows run may still find
something, and `WINDOWS-CHECK.md` says what to look at.

## Before the first real build: reserve the name (owner)

Partner Center › Apps › New product › MSIX or PWA app › reserve **CadCraft Studio**. Then Product management › Product
identity shows three values; put them where the build can read them (never in the repo):

```
MSIX_IDENTITY_NAME=<Package/Identity/Name>            e.g. TheDataDesignGroup.CadCraftStudio
MSIX_PUBLISHER=<Package/Identity/Publisher>            the whole "CN=…" string
MSIX_PUBLISHER_DISPLAY=<Package/Properties/PublisherDisplayName>
```

Without them the build still makes a layout, with a visibly fake identity (`CN=UNRESERVED-NOT-FOR-THE-STORE`) and a warning:
good for testing the layout, refused by Partner Center.

## Pack and check (a Windows machine with the Windows SDK; PowerShell)

```powershell
$sdk = (Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin\10.*\x64\makeappx.exe" | Sort-Object FullName | Select-Object -Last 1).DirectoryName
$layout = "out\msix\CadCraft-Studio-<version>"          # the folder build.mjs printed
& "$sdk\makeappx.exe" pack /d $layout /p "out\CadCraft-Studio-<version>.msix" /o
& "$sdk\makeappx.exe" unpack /p "out\CadCraft-Studio-<version>.msix" /d "$env:TEMP\msix-check" /o   # it reads back
```

Sideload test (a throwaway certificate; the Store does the real signing):

```powershell
$cert = New-SelfSignedCertificate -Type Custom -Subject "<the MSIX_PUBLISHER string>" -KeyUsage DigitalSignature `
  -FriendlyName "CadCraft Studio test" -CertStoreLocation "Cert:\CurrentUser\My" `
  -TextExtension @("2.5.29.37={text}1.3.6.1.5.5.7.3.3", "2.5.29.19={text}")
& "$sdk\signtool.exe" sign /fd SHA256 /a /sha1 $cert.Thumbprint "out\CadCraft-Studio-<version>.msix"
# trust it once (an elevated prompt), then install: double-click the .msix, or
Add-AppxPackage "out\CadCraft-Studio-<version>.msix"
```

The test certificate's Subject must equal the manifest's Publisher exactly, so a stand-in layout needs
`MSIX_PUBLISHER` set to that subject, or the signature is refused. Never publish a self-signed package.

Windows App Certification Kit (the Store's own validator, the same one Partner Center runs after upload):

```powershell
& "${env:ProgramFiles(x86)}\Windows Kits\10\App Certification Kit\appcert.exe" reset
& "${env:ProgramFiles(x86)}\Windows Kits\10\App Certification Kit\appcert.exe" test -appxpackagepath "out\CadCraft-Studio-<version>.msix" -reportoutputpath "out\wack.xml"
```

Paste `wack.xml` (or its failures) back to the platforms log.

## What differs from the installer (say it in the Store description)

- **Uninstall removes Studio's own settings and recovery copies**: AppData is redirected into the package. Files people
  saved are in their own folders and stay.
- **File types**: Windows never makes a package the default silently. The first double-click on a `.dwg` asks which program;
  "Always" makes Studio the default. The installer's "our own type is the default" rule does not exist here.
- **The 0.4 per-app installs** are not removed at install (an MSIX can't run an uninstaller). The first-start move
  (`carry.rs`'s question, SD.8) already offers to carry their data; removing the old installs from inside the package is
  **not built** and waits for a Windows machine (Studio would ask, then run each old uninstaller).
- **WebView2** is the system's (always on Windows 11 and on updated Windows 10): the package is ~20 MB, not ~240 MB.
- **Updates**: the Store package from the Store, the website package through App Installer; Studio's Check for updates asks
  Windows for both (src-tauri/src/package.rs) and never runs the file-replacing updater inside a package. A package installed
  from a bare `.msix` file (no `.appinstaller`, not the Store) has no update source: the check says so (`not_store`).

## CI

`.github/workflows/msix.yml` (run by hand) does the Windows half on a GitHub runner: builds the layout natively, packs it,
signs a throwaway copy, installs it, runs WACK and uploads the `.msix` (unsigned, for Partner Center) and the WACK report.
