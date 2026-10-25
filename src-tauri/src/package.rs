// SPDX-License-Identifier: Apache-2.0
//! Studio as an MSIX package (0.7 MSIX): where its updates come from, and asking Windows for them.
//!
//! A packaged Studio must never run the Tauri updater: that one swaps files in the install folder, which a package's folder
//! (WindowsApps) does not allow, and it would leave the package's own record behind. Windows updates a package itself:
//!
//! - **Microsoft Store** (the package is Store-signed, `Package.SignatureKind == Store`): `StoreContext`
//!   `GetAppAndOptionalStorePackageUpdatesAsync`, then `RequestDownloadAndInstallStorePackageUpdatesAsync` (the Store's own
//!   progress window; it closes and reopens Studio). The window handle goes in through `IInitializeWithWindow`.
//! - **getcadcraft.com** (installed from an `.appinstaller`, `Package.GetAppInstallerInfo()` is set): `CheckUpdateAvailabilityAsync`
//!   asks the same `.appinstaller` App Installer reads at start (OnLaunch); on yes `PackageManager.AddPackageByAppInstallerFileAsync`
//!   with `ForceTargetAppShutdown`. Studio registers for restart first (`RegisterApplicationRestart`), so Windows starts it
//!   again on the new version.
//! - **Anything else** (a package installed from a file, a developer registration): nothing can update it; the check answers
//!   `not_store` so the hub can say why.
//!
//! Failures use updates.rs's codes: `network`, `feed`, `install`, `not_store`.

/// Where a packaged Studio's updates come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    Store,
    AppInstaller,
    /// Installed from a package file: no update source.
    Sideloaded,
}

impl Route {
    pub fn name(self) -> &'static str {
        match self {
            Route::Store => "store",
            Route::AppInstaller => "appinstaller",
            Route::Sideloaded => "sideloaded",
        }
    }
}

/// The decision, from what Windows says about the package (pure, so it is tested everywhere). The Store's signature wins:
/// a Store install that also remembers an `.appinstaller` is still updated by the Store.
pub fn route_for(store_signed: bool, has_appinstaller: bool) -> Route {
    if store_signed {
        Route::Store
    } else if has_appinstaller {
        Route::AppInstaller
    } else {
        Route::Sideloaded
    }
}

/// What a package check found.
#[derive(Debug, Default, Clone)]
pub struct Found {
    /// The version on offer, when Windows says (Store) or the `.appinstaller` names it.
    pub version: Option<String>,
}

/// A Store failure on a package the Store did not sign: the Store doesn't know this copy (e.g. 0x803FB112 on a sideloaded
/// package, workstation11a 2026-10-25), so the honest answer is `not_store`, whatever the Store's code.
pub fn store_failure(store_signed: bool, msg: String) -> String {
    if store_signed {
        msg
    } else {
        format!("not_store: the Microsoft Store does not know this copy ({})", msg.split_once(": ").map(|(_, t)| t).unwrap_or(&msg).trim())
    }
}

/// Win32 / WinRT failures as updates.rs codes: WinINet / WinHTTP / HTTP status errors are the network; the rest is the install.
pub fn hresult_code(hr: u32) -> &'static str {
    match hr {
        0x8007_2EE0..=0x8007_2F8F => "network",   // ERROR_WINHTTP_* / ERROR_INTERNET_* (name not resolved, timeout, cannot connect)
        0x8019_0000..=0x8019_FFFF => "network",   // BG_E_HTTP_ERROR_* (the server answered 4xx/5xx)
        0x8007_0035 | 0x8007_0043 | 0x8007_04CF | 0x8007_04D0 | 0x8007_04D1 => "network", // the path / network not found or unreachable
        0x8007_0005 => "install",                 // access denied
        _ => "install",
    }
}

/// `Version="1.2.3.4"` of the `<MainPackage>` (or `<MainBundle>`) in an `.appinstaller` file.
pub fn appinstaller_version(xml: &str) -> Option<String> {
    for tag in ["<MainPackage", "<MainBundle"] {
        if let Some(i) = xml.find(tag) {
            let rest = &xml[i..];
            let end = rest.find('>').unwrap_or(rest.len());
            let head = &rest[..end];
            if let Some(j) = head.find(" Version=\"") {
                let v = &head[j + 10..];
                return v.find('"').map(|k| v[..k].to_string());
            }
        }
    }
    None
}

/// x.y.z.0 → x.y.z (how the hub shows a package version; MSIX keeps the fourth part at 0).
pub fn short_version(v: &str) -> String {
    v.strip_suffix(".0").filter(|s| s.matches('.').count() == 2).unwrap_or(v).to_string()
}

#[cfg(windows)]
pub use imp::*;

#[cfg(windows)]
mod imp {
    use super::{hresult_code, route_for, short_version, Found, Route};
    use windows::core::{Interface, HSTRING};
    use windows::ApplicationModel::{Package, PackageSignatureKind, PackageUpdateAvailability};
    use windows::Foundation::Uri;
    use windows::Management::Deployment::{AddPackageByAppInstallerOptions, PackageManager};
    use windows::Services::Store::{StoreContext, StorePackageUpdateState};
    use windows::Win32::Foundation::{HWND, WIN32_ERROR};
    use windows::Win32::Storage::Packaging::Appx::GetCurrentPackageFullName;
    use windows::Win32::System::Recovery::{RegisterApplicationRestart, REGISTER_APPLICATION_RESTART_FLAGS};
    use windows::Win32::UI::Shell::IInitializeWithWindow;

    const APPMODEL_ERROR_NO_PACKAGE: u32 = 15700;

    fn code(e: &windows::core::Error) -> String {
        let hr = e.code().0 as u32;
        format!("{}: 0x{hr:08X} {}", hresult_code(hr), e.message())
    }

    /// True when this process runs with package identity (installed from an MSIX).
    pub fn packaged() -> bool {
        let mut len = 0u32;
        let r: WIN32_ERROR = unsafe { GetCurrentPackageFullName(&mut len, None) };
        r.0 != APPMODEL_ERROR_NO_PACKAGE
    }

    /// The package's full name (for the log and the test evidence).
    #[allow(dead_code)]
    pub fn full_name() -> Option<String> {
        Package::Current().ok()?.Id().ok()?.FullName().ok().map(|s| s.to_string())
    }

    fn appinstaller_uri(p: &Package) -> Option<Uri> {
        p.GetAppInstallerInfo().ok().and_then(|i| i.Uri().ok())
    }

    fn store_signed() -> bool {
        Package::Current().and_then(|p| p.SignatureKind()).map(|k| k == PackageSignatureKind::Store).unwrap_or(false)
    }

    pub fn route() -> Route {
        let Ok(p) = Package::Current() else { return Route::Sideloaded };
        let store = p.SignatureKind().map(|k| k == PackageSignatureKind::Store).unwrap_or(false);
        route_for(store, appinstaller_uri(&p).is_some())
    }

    /// The installed package's version as x.y.z.w.
    pub fn version() -> Option<String> {
        let v = Package::Current().ok()?.Id().ok()?.Version().ok()?;
        Some(format!("{}.{}.{}.{}", v.Major, v.Minor, v.Build, v.Revision))
    }

    fn store(hwnd: isize) -> Result<StoreContext, String> {
        let ctx = StoreContext::GetDefault().map_err(|e| code(&e))?;
        // A desktop program must hand the Store its window, or the Store's dialogs have no owner and fail.
        if hwnd != 0 {
            if let Ok(init) = ctx.cast::<IInitializeWithWindow>() {
                unsafe { init.Initialize(HWND(hwnd as *mut core::ffi::c_void)) }.map_err(|e| code(&e))?;
            }
        }
        Ok(ctx)
    }

    /// Is there an update? Ok(None) = up to date. Blocking: call from a worker thread.
    pub fn check(route: Route, hwnd: isize) -> Result<Option<Found>, String> {
        match route {
            Route::Sideloaded => Err("not_store: this package was not installed from the Microsoft Store or from an .appinstaller file".into()),
            Route::Store => {
                let signed = store_signed();
                let ctx = store(hwnd).map_err(|m| super::store_failure(signed, m))?;
                let list = ctx.GetAppAndOptionalStorePackageUpdatesAsync().and_then(|op| op.join()).map_err(|e| super::store_failure(signed, code(&e)))?;
                if list.Size().unwrap_or(0) == 0 {
                    return Ok(None);
                }
                let version = list.GetAt(0).ok().and_then(|u| u.Package().ok()).and_then(|p| p.Id().ok()).and_then(|id| id.Version().ok())
                    .map(|v| short_version(&format!("{}.{}.{}.{}", v.Major, v.Minor, v.Build, v.Revision)));
                Ok(Some(Found { version }))
            }
            Route::AppInstaller => {
                let p = Package::Current().map_err(|e| code(&e))?;
                let r = p.CheckUpdateAvailabilityAsync().and_then(|op| op.join()).map_err(|e| code(&e))?;
                let a = r.Availability().map_err(|e| code(&e))?;
                if a == PackageUpdateAvailability::NoUpdates {
                    return Ok(None);
                }
                if a == PackageUpdateAvailability::Available || a == PackageUpdateAvailability::Required {
                    // Which version: read the same .appinstaller (only to show it; Windows decides what installs).
                    let version = appinstaller_uri(&p).and_then(|u| u.ToString().ok()).and_then(|u| fetch(&u.to_string()))
                        .and_then(|xml| super::appinstaller_version(&xml)).map(|v| short_version(&v));
                    return Ok(Some(Found { version }));
                }
                let detail = r.ExtendedError().map(|h| format!(" 0x{:08X}", h.0 as u32)).unwrap_or_default();
                let hr = r.ExtendedError().map(|h| h.0 as u32).unwrap_or(0);
                let c = if hr == 0 { "feed" } else { hresult_code(hr) };
                Err(format!("{c}: App Installer could not read the update file{detail} (availability {})", a.0))
            }
        }
    }

    fn fetch(url: &str) -> Option<String> {
        if url.starts_with("http://") || url.starts_with("https://") {
            ureq::get(url).call().ok()?.body_mut().read_to_string().ok()
        } else {
            let path = url.strip_prefix("file:///").map(|p| p.replace('/', "\\")).unwrap_or_else(|| url.to_string());
            std::fs::read_to_string(path).ok()
        }
    }

    /// Install the update. Store: the Store's own window (Studio may be closed by it). App Installer: Windows shuts Studio
    /// down, replaces the package and starts it again (restart registered first). Blocking.
    pub fn install(route: Route, hwnd: isize) -> Result<(), String> {
        match route {
            Route::Sideloaded => Err("not_store: this package has no update source".into()),
            Route::Store => {
                let ctx = store(hwnd)?;
                let list = ctx.GetAppAndOptionalStorePackageUpdatesAsync().and_then(|op| op.join()).map_err(|e| code(&e))?;
                if list.Size().unwrap_or(0) == 0 {
                    return Ok(());
                }
                let r = ctx.RequestDownloadAndInstallStorePackageUpdatesAsync(&list).and_then(|op| op.join()).map_err(|e| code(&e))?;
                let s = r.OverallState().map_err(|e| code(&e))?;
                if s == StorePackageUpdateState::Completed {
                    Ok(())
                } else if s == StorePackageUpdateState::Canceled {
                    Err("declined: the Store update was cancelled".into())
                } else {
                    Err(format!("install: the Store update ended in state {}", s.0))
                }
            }
            Route::AppInstaller => {
                let p = Package::Current().map_err(|e| code(&e))?;
                let uri = appinstaller_uri(&p).ok_or_else(|| "feed: the package has no .appinstaller".to_string())?;
                // Started again by Windows after the update (no command line: the hub opens as after a normal start).
                let _ = unsafe { RegisterApplicationRestart(&HSTRING::new(), REGISTER_APPLICATION_RESTART_FLAGS(0)) };
                let pm = PackageManager::new().map_err(|e| code(&e))?;
                let opt = AddPackageByAppInstallerOptions::ForceTargetAppShutdown;
                // No target volume (Windows' default): asking for the default volume needs the packageManagement capability.
                // AddPackageByAppInstallerFileAsync may need that capability too; when Windows says "access denied" the
                // Request… form asks the person through App Installer instead and needs none (Windows 10 2004+).
                let denied = |hr: u32| hr == 0x8007_0005;
                let first = pm.AddPackageByAppInstallerFileAsync(&uri, opt, None::<&windows::Management::Deployment::PackageVolume>).and_then(|op| op.join());
                let r = match first {
                    Ok(r) if !denied(r.ExtendedErrorCode().map(|h| h.0 as u32).unwrap_or(0)) => r,
                    Err(e) if !denied(e.code().0 as u32) => return Err(code(&e)),
                    _ => pm
                        .RequestAddPackageByAppInstallerFileAsync(&uri, opt, None::<&windows::Management::Deployment::PackageVolume>)
                        .and_then(|op| op.join())
                        .map_err(|e| code(&e))?,
                };
                let hr = r.ExtendedErrorCode().map(|h| h.0 as u32).unwrap_or(0);
                if hr != 0 {
                    let text = r.ErrorText().map(|s| s.to_string()).unwrap_or_default();
                    return Err(format!("{}: 0x{hr:08X} {text}", hresult_code(hr)));
                }
                Ok(())
            }
        }
    }
}

/// Not Windows: never packaged.
#[cfg(not(windows))]
pub fn packaged() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_store_signature_decides_first() {
        assert_eq!(route_for(true, false), Route::Store);
        assert_eq!(route_for(true, true), Route::Store);
        assert_eq!(route_for(false, true), Route::AppInstaller);
        assert_eq!(route_for(false, false), Route::Sideloaded);
    }

    #[test]
    fn windows_errors_map_to_the_hub_codes() {
        assert_eq!(hresult_code(0x8007_2EE7), "network"); // name not resolved
        assert_eq!(hresult_code(0x8019_0194), "network"); // HTTP 404
        assert_eq!(hresult_code(0x8007_0005), "install");
        assert_eq!(hresult_code(0x80073CF3), "install"); // the package failed its update checks
    }

    #[test]
    fn a_store_error_on_a_copy_the_store_did_not_sign_is_not_store() {
        assert_eq!(store_failure(false, "install: 0x803FB112 ".into()), "not_store: the Microsoft Store does not know this copy (0x803FB112)");
        assert_eq!(store_failure(true, "network: 0x80072EE7 x".into()), "network: 0x80072EE7 x");
    }

    #[test]
    fn the_appinstaller_names_its_version() {
        let x = r#"<AppInstaller Uri="https://x/a" Version="0.7.1.0"><MainPackage Name="N" Publisher="CN=x" Version="0.7.1.0" ProcessorArchitecture="x64" Uri="https://x/b.msix" /></AppInstaller>"#;
        assert_eq!(appinstaller_version(x).as_deref(), Some("0.7.1.0"));
        assert_eq!(short_version("0.7.1.0"), "0.7.1");
        assert_eq!(short_version("0.7.1.3"), "0.7.1.3");
        assert_eq!(appinstaller_version("<AppInstaller/>"), None);
    }
}
