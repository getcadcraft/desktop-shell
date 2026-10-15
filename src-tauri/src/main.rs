// SPDX-License-Identifier: Apache-2.0
// The desktop executable: everything lives in the library (src/lib.rs), which Android and iPad load as well (TB.1).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    cadcraft_desktop_lib::run()
}
