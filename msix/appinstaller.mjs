// SPDX-License-Identifier: Apache-2.0
// msix/appinstaller.mjs — the .appinstaller file for the website's MSIX (0.7 MSIX). Pure: data in, XML out.
//
// Windows' App Installer reads it to install Studio and, from then on, to update it: the file's own Uri is where it looks for
// a newer one. getcadcraft.com serves that file from the update service (`/api/v1/updates/studio.appinstaller`,
// UpdateFeed::appInstaller), so App Installer's checks follow the same rules as every other edition; this generator is the
// same shape, for the build's own copy next to the .msix (tests, and a local install from a folder).
//
//   OnLaunch HoursBetweenUpdateChecks=… ShowPrompt=true  → when Studio starts, at most every N hours, App Installer looks and
//                                                          asks before updating (UpdateBlocksActivation=false: Studio opens
//                                                          meanwhile, the update applies next time)
//   AutomaticBackgroundTask                               → Windows also looks every 8 hours in the background
//   ForceUpdateFromAnyVersion=false                        → never "updates" to an older version
// Studio's own "Check for updates" asks the same file through Package.CheckUpdateAvailabilityAsync (src-tauri/src/package.rs).
import { msixVersion } from './manifest.mjs';

const esc = (s) => String(s).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');

/**
 * @param {object} o
 * @param {string} o.uri          where this .appinstaller is served (App Installer re-reads it from here)
 * @param {string} o.packageUri   the .msix for this version
 * @param {string} o.identityName the package's Identity Name
 * @param {string} o.publisher    its Publisher ("CN=…", the signing certificate's subject)
 * @param {string} o.version      x.y.z (→ x.y.z.0)
 * @param {number} [o.hours]      HoursBetweenUpdateChecks (0–255; default 12)
 */
export function appInstaller(o) {
  for (const k of ['uri', 'packageUri', 'identityName', 'publisher', 'version']) if (!o[k]) throw new Error(`appinstaller: ${k} is required`);
  for (const k of ['uri', 'packageUri']) if (!/^(https?:\/\/|file:\/\/\/|\\\\)/.test(o[k])) throw new Error(`appinstaller: ${k} must be http(s), file:/// or a share`);
  // Studio updates the package with PackageManager.AddPackageByAppInstallerFileAsync, which refuses a link whose path does not end
  // in .appinstaller (0x80070057, workstation11a 2026-10-25), so the file's own Uri must.
  if (!/\.appinstaller$/i.test(o.uri.split(/[?#]/)[0])) throw new Error('appinstaller: uri must end in .appinstaller (Windows refuses it otherwise)');
  const hours = Number.isInteger(o.hours) ? o.hours : 12;
  if (hours < 0 || hours > 255) throw new Error('appinstaller: hours is 0–255');
  const v = msixVersion(o.version);
  return [
    '<?xml version="1.0" encoding="utf-8"?>',
    `<AppInstaller xmlns="http://schemas.microsoft.com/appx/appinstaller/2021" Version="${v}" Uri="${esc(o.uri)}">`,
    `  <MainPackage Name="${esc(o.identityName)}" Publisher="${esc(o.publisher)}" Version="${v}" ProcessorArchitecture="x64" Uri="${esc(o.packageUri)}" />`,
    '  <UpdateSettings>',
    `    <OnLaunch HoursBetweenUpdateChecks="${hours}" ShowPrompt="true" UpdateBlocksActivation="false" />`,
    '    <AutomaticBackgroundTask />',
    '    <ForceUpdateFromAnyVersion>false</ForceUpdateFromAnyVersion>',
    '  </UpdateSettings>',
    '</AppInstaller>',
    '',
  ].join('\r\n');
}
