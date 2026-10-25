// SPDX-License-Identifier: Apache-2.0
// The MSIX manifest and package layout (0.7 MSIX). No Windows needed: the manifest is data → XML, the layout is a folder.
// What only Windows can say (MakeAppx accepts it, the Windows App Certification Kit passes it) is msix/PACK.md's run.
//   node test/msix.test.mjs
import fs from 'node:fs';
import crypto from 'node:crypto';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { manifest, msixVersion, ftaName, appId, tiles } from '../msix/manifest.mjs';
import { buildLayout, checkLayout, identityFrom } from '../msix/layout.mjs';
import { pack, unpackCheck, partName } from '../msix/pack.mjs';
import { appInstaller } from '../msix/appinstaller.mjs';
import zlib from 'node:zlib';

let pass = 0, fail = 0;
const ok = (c, m, extra) => { if (c) pass++; else { fail++; console.log('  ✗ ' + m + (extra ? ' ' + extra : '')); } };
const HERE = path.dirname(new URL(import.meta.url).pathname);
const APPS = JSON.parse(fs.readFileSync(path.join(HERE, '..', 'apps.json'), 'utf8'));
const ALL = ['cadcraft', 'bimcraft', 'modelcraft', 'markupcraft', 'takeoffcraft', 'civilcraft', 'detailcraft', 'partcraft'];
const NAMES = { cadcraft: 'CadCraft', bimcraft: 'BimCraft', modelcraft: 'ModelCraft', markupcraft: 'MarkupCraft', takeoffcraft: 'TakeoffCraft', civilcraft: 'CivilCraft', detailcraft: 'DetailCraft', partcraft: 'PartCraft' };
const apps = ALL.map((s) => ({ slug: s, name: NAMES[s], role: 'role of ' + s }));
const exts = [...new Set(ALL.flatMap((a) => [...APPS[a].files.own, ...APPS[a].files.polite, ...APPS[a].files.openWith]))];
const fileTypes = exts.map((ext) => ({ ext, label: APPS._types[ext] || ext.toUpperCase() + ' file' }));
const id = { identityName: 'TheDataDesignGroup.CadCraftStudio', publisher: 'CN=A1B2C3D4-0000-1111-2222-333344445555', publisherDisplay: 'The Data Design Group, Inc.' };
const make = (extra = {}) => manifest({ ...id, version: '0.7.0', displayName: 'CadCraft Studio', exe: 'CadCraft Studio.exe', apps, fileTypes, description: 'All eight apps.', ...extra });

ok(msixVersion('0.7') === '0.7.0.0' && msixVersion('0.7.2') === '0.7.2.0' && msixVersion('0.7.2.0') === '0.7.2.0', 'versions: x.y and x.y.z become x.y.z.0 (the Store wants the last part 0)');
let e = ''; try { msixVersion('dev'); } catch (x) { e = x.message; } ok(/x\.y/.test(e), 'dev is not a package version');
ok(ftaName('LandXML') === 'cclandxml' && ftaName('bcf.zip') === 'ccbcfzip', 'file type names are lower-case letters and digits');
ok(appId('cadcraft') === 'cadcraft' && appId('3d') === 'A3d', 'application ids start with a letter');

const xml = make();
// Well-formed XML, read by a real parser (python's), and the shape we rely on.
const py = `
import sys, xml.etree.ElementTree as ET, json
ns = {'f': 'http://schemas.microsoft.com/appx/manifest/foundation/windows10', 'uap': 'http://schemas.microsoft.com/appx/manifest/uap/windows10', 'uap10': 'http://schemas.microsoft.com/appx/manifest/uap/windows10/10', 'd2': 'http://schemas.microsoft.com/appx/manifest/desktop/windows10/2', 'r': 'http://schemas.microsoft.com/appx/manifest/foundation/windows10/restrictedcapabilities'}
t = ET.parse(sys.argv[1]).getroot()
ident = t.find('f:Identity', ns).attrib
apps = [(a.get('Id'), a.get('{%s}Parameters' % ns['uap10']), a.get('Executable')) for a in t.findall('f:Applications/f:Application', ns)]
ftas = [x.get('Name') for x in t.iter('{%s}FileTypeAssociation' % ns['uap'])]
types = [x.text for x in t.iter('{%s}FileType' % ns['uap'])]
proto = [x.get('Name') for x in t.iter('{%s}Protocol' % ns['uap'])]
rules = [(x.get('Direction'), x.get('IPProtocol'), x.get('Profile')) for x in t.iter('{%s}Rule' % ns['d2'])]
caps = [c.get('Name') for c in t.iter() if c.tag.endswith('}Capability')]
print(json.dumps({'ident': ident, 'apps': apps, 'ftas': ftas, 'types': types, 'proto': proto, 'rules': rules, 'caps': caps}))
`;
const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'cc-msix-'));
const parse = (text) => {
  const f = path.join(tmp, 'm.xml'); fs.writeFileSync(f, text);
  const r = spawnSync('python3', ['-I', '-c', py, f], { encoding: 'utf8' });
  return r.status === 0 ? JSON.parse(r.stdout) : { error: r.stderr };
};
const m = parse(xml);
ok(!m.error, 'the manifest is well-formed XML', m.error);
ok(m.ident.Name === id.identityName && m.ident.Publisher === id.publisher && m.ident.Version === '0.7.0.0' && m.ident.ProcessorArchitecture === 'x64', 'identity: the reserved name and publisher, version x.y.z.0, x64');
ok(m.apps.length === 9 && m.apps[0][0] === 'Studio' && m.apps[0][1] === null, 'nine applications: Studio, then one per app');
ok(ALL.every((s, i) => m.apps[i + 1][0] === appId(s) && m.apps[i + 1][1] === `--app=${s}`), 'each app tile starts the same program with --app=<slug>');
ok(m.apps.every((a) => a[2] === 'CadCraft Studio.exe'), 'every tile runs the one executable');
ok(m.types.length === exts.length && exts.every((x) => m.types.includes('.' + x)), `every extension Studio opens is declared (${exts.length})`);
ok(new Set(m.ftas).size === m.ftas.length, 'file type association names are unique');
ok(m.proto.join() === 'ccstudio', 'the ccstudio:// link is declared');
ok(m.rules.length === 2 && m.rules.every((r) => r[0] === 'in' && r[2] === 'private'), 'Live: inbound firewall rules on private networks only');
{ const i = xml.indexOf('windows.firewallRules'); ok(i > xml.indexOf('</Applications>') && xml.lastIndexOf('<Extensions>', i) > xml.indexOf('</Capabilities>'), 'the firewall rules are a package-level extension (Windows refuses them under an Application)'); }
ok(m.caps.includes('runFullTrust') && m.caps.includes('internetClient') && m.caps.includes('privateNetworkClientServer') && m.caps.length === 3, 'capabilities: full trust, internet, private network — nothing else');
ok(!/default/i.test(xml.replace(/DefaultTile/g, '')), 'no default is claimed for any file type (Windows asks the person once)');

// Refusals.
const bad = (o) => { try { make(o); return ''; } catch (x) { return x.message; } };
ok(/publisher/.test(bad({ publisher: 'The Data Design Group' })), 'a publisher that is not CN=… is refused');
ok(/package name/.test(bad({ identityName: 'x' })), 'a bad identity name is refused');
ok(/clashes/.test(bad({ apps: [apps[0], apps[0]] })), 'two apps with one id are refused');
ok(/required/.test(bad({ exe: '' })), 'a missing executable is refused');
ok(/&amp;/.test(make({ publisherDisplay: 'A & B' })) && !/A & B/.test(make({ publisherDisplay: 'A & B' })), 'text is XML-escaped');

// Tiles the manifest names.
ok(tiles(apps).length === 4 + 16 && tiles(apps).every(([f]) => /\.png$/.test(f)), 'the tile list: Studio (4) + two per app');

// identity from the environment: all three or none.
ok(identityFrom({}).reserved === false && /UNRESERVED/.test(identityFrom({}).publisher), 'no reservation: a visible stand-in, flagged');
ok(identityFrom({ MSIX_IDENTITY_NAME: 'A.B', MSIX_PUBLISHER: 'CN=x', MSIX_PUBLISHER_DISPLAY: 'X' }).reserved === true, 'all three set: the reservation');
e = ''; try { identityFrom({ MSIX_IDENTITY_NAME: 'A.B' }); } catch (x) { e = x.message; } ok(/together/.test(e), 'one or two of the three: refused');

// A layout from a stand-in program and edition (Pillow draws the tiles).
const stage = path.join(tmp, 'stage'); fs.mkdirSync(path.join(stage, 'app'), { recursive: true });
fs.writeFileSync(path.join(stage, 'app', 'index.html'), '<p>x</p>');
fs.writeFileSync(path.join(stage, 'shell.json'), JSON.stringify({ app: 'studio', name: 'CadCraft Studio', version: '0.7', account: false, store: true, suite: true }));
const exePath = path.join(tmp, 'CadCraft Studio.exe'); fs.writeFileSync(exePath, 'MZ');
const png = path.join(tmp, 'icon.png');
spawnSync('python3', ['-I', '-c', 'from PIL import Image; Image.new("RGBA", (512, 512), (200, 30, 30, 255)).save(__import__("sys").argv[1])', png]);
const lay = buildLayout({ out: tmp, name: 'CadCraft Studio', version: '0.7.0', exe: 'CadCraft Studio.exe', exePath, stage, apps, fileTypes, shellDir: tmp, iconFor: () => png, identity: identityFrom({}) });
ok(fs.existsSync(path.join(lay, 'AppxManifest.xml')) && fs.existsSync(path.join(lay, 'app/index.html')) && fs.existsSync(path.join(lay, 'CadCraft Studio.exe')), 'the layout has the manifest, app/ and the program');
ok(checkLayout(lay).length === 0, 'the layout check passes: every named file exists, store build, no update feed', JSON.stringify(checkLayout(lay)));
const size = (f) => spawnSync('python3', ['-I', '-c', 'from PIL import Image; import sys; print(*Image.open(sys.argv[1]).size)', path.join(lay, 'Assets', f)], { encoding: 'utf8' }).stdout.trim();
ok(size('Studio-wide.png') === '310 150' && size('StoreLogo.png') === '50 50' && size('cadcraft-44.png') === '44 44', 'tiles are cut to the sizes the manifest declares');
fs.rmSync(path.join(lay, 'Assets', 'Studio-150.png'));
ok(checkLayout(lay).some((p) => /Studio-150/.test(p)), 'a missing tile is caught');
fs.writeFileSync(path.join(lay, 'shell.json'), JSON.stringify({ updates: { endpoint: 'x' }, account: true }));
ok(checkLayout(lay).length >= 3, 'an update feed, sign-in on and a non-store build are all caught');


// The website package (0.7): its own identity (our certificate's subject), sign-in on, edition msix-web (App Installer updates it).
ok(identityFrom({}, 'web').reserved === false && /NO-SIGNING-CERTIFICATE/.test(identityFrom({}, 'web').publisher), 'website package without MSIX_WEB_PUBLISHER: a visible stand-in');
const wid = identityFrom({ MSIX_WEB_PUBLISHER: 'CN=The Data Design Group, Inc., O=The Data Design Group, Inc., C=US' }, 'web');
ok(wid.reserved && wid.identityName === 'CadCraft.Studio' && /^CN=The Data Design Group/.test(wid.publisher), 'website identity: CadCraft.Studio, publisher = the Azure certificate subject');
ok(identityFrom({ MSIX_IDENTITY_NAME: 'A.B', MSIX_PUBLISHER: 'CN=store', MSIX_PUBLISHER_DISPLAY: 'X' }).publisher === 'CN=store', 'the Store identity stays Partner Center\'s');
const webShell = { app: 'studio', name: 'CadCraft Studio', version: '0.7', suite: true, updates: { edition: 'msix-web', auto: false } };
const wl = buildLayout({ out: tmp, name: 'CadCraft Studio', version: '0.7.0', exe: 'CadCraft Studio.exe', exePath, stage, apps, fileTypes, shellDir: tmp, iconFor: () => png, identity: wid, kind: 'web', shell: webShell });
ok(checkLayout(wl, 'web').length === 0 && path.basename(wl) === 'CadCraft-Studio-0.7.0', 'the website layout passes its check', JSON.stringify(checkLayout(wl, 'web')));
ok(/Publisher="CN=The Data Design Group, Inc\., O=The Data Design Group, Inc\., C=US"/.test(fs.readFileSync(path.join(wl, 'AppxManifest.xml'), 'utf8')), 'the manifest carries the website publisher');
fs.writeFileSync(path.join(wl, 'shell.json'), JSON.stringify({ ...webShell, updates: undefined }));
ok(checkLayout(wl, 'web').some((p) => /msix-web/.test(p)), 'a website package without its edition is caught');
fs.writeFileSync(path.join(wl, 'shell.json'), JSON.stringify(webShell));
const storeLay = buildLayout({ out: tmp, name: 'CadCraft Studio', version: '0.7.0', exe: 'CadCraft Studio.exe', exePath, stage, apps, fileTypes, shellDir: tmp, iconFor: () => png, identity: identityFrom({}), kind: 'store' });
fs.writeFileSync(path.join(storeLay, 'shell.json'), JSON.stringify({ app: 'studio', account: false, store: true, updates: { edition: 'msix-store' } }));
ok(checkLayout(storeLay, 'store').length === 0, 'the Store package may say its edition (msix-store, no endpoint)');
fs.writeFileSync(path.join(storeLay, 'shell.json'), JSON.stringify({ app: 'studio', account: false, store: true, updates: { edition: 'msix-store', endpoint: 'https://x' } }));
ok(checkLayout(storeLay, 'store').some((p) => /Store updates a package/.test(p)), 'but never with an endpoint: the Store updates it');

// Packing on Linux (msix/pack.mjs): OPC names, a block map with every 64 KiB block, deflate per block, read back whole.
ok(partName('CadCraft Studio.exe') === 'CadCraft%20Studio.exe' && partName('app/a b/ü.js') === 'app/a%20b/%C3%BC.js' && partName('Assets\\x.png') === 'Assets/x.png', 'part names are percent-encoded with / separators');
fs.writeFileSync(path.join(wl, 'app', 'big.js'), crypto.randomBytes(40000).toString('hex'));   // 80 000 bytes: two blocks, compressible
fs.writeFileSync(path.join(wl, 'app', 'empty.txt'), '');
fs.writeFileSync(path.join(wl, 'app', 'LICENSE'), 'no extension');
const msix = path.join(tmp, 'CadCraft Studio-0.7.0.msix');
const pk = pack(wl, msix);
ok(pk.files > 20 && fs.statSync(msix).size === pk.bytes, 'the package is written', JSON.stringify(pk));
ok(unpackCheck(msix).length === 0, 'it reads back: every block hash, size, LfhSize and the content types', JSON.stringify(unpackCheck(msix)));
const zl = spawnSync('unzip', ['-l', msix], { encoding: 'utf8' });
ok(zl.status === 0 && /CadCraft%20Studio\.exe/.test(zl.stdout) && /AppxBlockMap\.xml/.test(zl.stdout) && /\[Content_Types\]\.xml/.test(zl.stdout), 'a standard unzip reads it (names as OPC parts)', zl.stderr);
const t = spawnSync('unzip', ['-tq', msix], { encoding: 'utf8' });
ok(t.status === 0, 'every entry\'s CRC checks out (unzip -t)', t.stdout + t.stderr);
const bmx = spawnSync('unzip', ['-p', msix, 'AppxBlockMap.xml'], { encoding: 'utf8' }).stdout;
ok(/<File Name="app\\big\.js" Size="80000" LfhSize="\d+"><Block Hash="[^"]+" Size="\d+"\/><Block Hash="[^"]+" Size="\d+"\/><\/File>/.test(bmx), 'a two-block file: two blocks, each with its compressed size');
ok(/<File Name="app\\empty\.txt" Size="0" LfhSize="\d+"><\/File>/.test(bmx) && /<File Name="AppxManifest\.xml"/.test(bmx), 'an empty file has no block; the manifest is in the map');
ok(/Override PartName="\/app\/LICENSE"/.test(spawnSync('unzip', ['-p', msix, '[[]Content_Types].xml'], { encoding: 'utf8' }).stdout), 'a file without an extension gets its own content type');
// Tamper one byte of a payload file: the check catches it.
const buf = fs.readFileSync(msix); const at = buf.indexOf(Buffer.from([0x89, 0x50, 0x4e, 0x47])); buf[at + 40] ^= 0xff; fs.writeFileSync(msix + '.bad', buf);
ok(unpackCheck(msix + '.bad').some((p) => /hash differs/.test(p)), 'a changed byte is caught by its block hash');

// The .appinstaller (the build's copy; the site serves the live one with the same shape).
const ai = appInstaller({ uri: 'https://getcadcraft.com/api/v1/updates/studio.appinstaller', packageUri: 'https://getcadcraft.com/downloads/0.7/CadCraft-Studio-0.7.msix', identityName: 'CadCraft.Studio', publisher: 'CN=A & B', version: '0.7.0' });
ok(/<AppInstaller xmlns="http:\/\/schemas\.microsoft\.com\/appx\/appinstaller\/2021" Version="0\.7\.0\.0" Uri="https:\/\/getcadcraft\.com\/api\/v1\/updates\/studio\.appinstaller">/.test(ai), 'the file names where it is served (App Installer re-reads it there)');
ok(/<MainPackage Name="CadCraft\.Studio" Publisher="CN=A &amp; B" Version="0\.7\.0\.0" ProcessorArchitecture="x64" Uri="https:\/\/getcadcraft\.com\/downloads\/0\.7\/CadCraft-Studio-0\.7\.msix" \/>/.test(ai), 'the package: identity, publisher (escaped), version, the .msix');
ok(/<OnLaunch HoursBetweenUpdateChecks="12" ShowPrompt="true" UpdateBlocksActivation="false" \/>/.test(ai) && /<AutomaticBackgroundTask \/>/.test(ai), 'checks at start (asking first, every 12 h) and in the background');
ok(/HoursBetweenUpdateChecks="0"/.test(appInstaller({ uri: 'http://127.0.0.1:8765/a.appinstaller', packageUri: 'http://127.0.0.1:8765/b.msix', identityName: 'A.B', publisher: 'CN=x', version: '0.7.1', hours: 0 })), 'a test file may check at every start');
{ let e2 = ''; try { appInstaller({ uri: 'https://x/api/v1/updates/appinstaller?edition=msix-web', packageUri: 'https://x/b.msix', identityName: 'A', publisher: 'CN=x', version: '0.7.0' }); } catch (x) { e2 = x.message; }
  ok(/\.appinstaller/.test(e2), 'the file\'s own link must end in .appinstaller (PackageManager refuses it otherwise: 0x80070057)'); }
let ae = ''; try { appInstaller({ uri: 'ftp://x.appinstaller', packageUri: 'https://x', identityName: 'A', publisher: 'CN=x', version: '0.7.0' }); } catch (x) { ae = x.message; }
ok(/must be/.test(ae), 'only http(s), file:/// or a share');

fs.rmSync(tmp, { recursive: true, force: true });
console.log(`${pass} passed, ${fail} failed`);
process.exit(fail ? 1 : 0);
