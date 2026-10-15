// swift-tools-version:5.5
// SPDX-License-Identifier: Apache-2.0
// The iPad half of the CadCraft shell's own mobile plugin (src-tauri/src/mobile.rs; docs/0.5-PART3.md TB.3).
// Linked into the app by src-tauri/build.rs on a Mac (tauri_utils::build::link_apple_library); never built elsewhere.

import PackageDescription

let package = Package(
  name: "cadcraft-plugin",
  platforms: [
    .iOS(.v15),
  ],
  products: [
    .library(name: "cadcraft-plugin", type: .static, targets: ["CadcraftPlugin"])
  ],
  dependencies: [
    // Copied there by build.rs from the tauri crate (DEP_TAURI_IOS_LIBRARY_PATH), as Tauri's own plugins do.
    .package(name: "Tauri", path: "../.tauri/tauri-api")
  ],
  targets: [
    .target(name: "CadcraftPlugin", dependencies: [.byName(name: "Tauri")], path: "Sources/CadcraftPlugin")
  ]
)
