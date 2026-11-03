# @getcadcraft/studio

Start **CadCraft Studio** on Linux with one command:

```sh
npx @getcadcraft/studio
npx @getcadcraft/studio plan.dwg      # open a file
```

CadCraft Studio is the eight CadCraft apps (CadCraft, BimCraft, ModelCraft, MarkupCraft, TakeoffCraft, CivilCraft,
DetailCraft, PartCraft) in one desktop install. It is free and open source (Apache-2.0).

## What it does

This package downloads and starts CadCraft Studio; it does not contain it. The first time you run it:

1. It asks getcadcraft.com which version of CadCraft Studio for Linux is current.
2. It downloads that version's AppImage from getcadcraft.com/downloads.
3. It checks the download twice: the SHA-256 against the release's manifest, and the signature against CadCraft's
   updater key (the same key CadCraft Studio's own updates are checked with). A file that fails either check is deleted, never run.
4. It keeps the AppImage in `~/.cache/cadcraft/studio/<version>/` and starts it.

After that it starts the cached copy, and downloads again only when a newer version is out. It keeps the newest two
versions and removes older ones. CadCraft Studio also checks for updates by itself once it is running.

## Options

| Option | |
|---|---|
| `--offline` | start the cached version without asking the site |
| `--where` | print the cache folder and the version in it |
| `--clean` | remove every cached version |
| `--quiet` | no progress messages |
| `--package-version` | this npm package's version (not CadCraft Studio's) |
| `-h`, `--help` | help |

Other arguments are passed to CadCraft Studio. Anything after `--` goes to it unchanged.

## Requirements

- Linux on 64-bit x86 (x86_64), with a desktop session (X11 or Wayland). ARM Linux is not built yet.
- Node.js 18.17 or later (for `npx`).
- About 300 MB in your cache folder per version.
- FUSE 2 (`libfuse2`) lets the AppImage mount itself. Without it this package tells the AppImage to unpack itself to a
  temporary folder and run from there, which is slower to start but works the same.

On Windows or macOS, download CadCraft Studio from https://getcadcraft.com/download. Every app also runs in the browser at
https://getcadcraft.com/use.

## Environment

| Variable | |
|---|---|
| `CADCRAFT_CACHE` | a different cache folder |
| `XDG_CACHE_HOME` | respected when `CADCRAFT_CACHE` is not set |
| `CADCRAFT_SITE` | a mirror of getcadcraft.com (https only). Downloads are still checked against CadCraft's key. |

## Source and tests

https://github.com/getcadcraft/desktop-shell (`npm/studio`). `npm test` runs the package against a local test site.
