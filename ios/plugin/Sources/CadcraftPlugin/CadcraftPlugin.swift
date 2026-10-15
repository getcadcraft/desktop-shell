// SPDX-License-Identifier: Apache-2.0
// CadcraftPlugin.swift — the iPad half of the shell's own mobile plugin (desktop-shell/src-tauri/src/mobile.rs; docs/0.5-PART3.md
// TB.3). The same commands as Android's CadcraftPlugin.kt, so the Rust side is one.
//
// Documents: the system's document picker; a pick is kept as a security-scoped bookmark (base64) — that is the "uri" Rust
// stores in the grant list, so recent files reopen after a restart. Rust reads and writes through file descriptors opened
// here while the bookmark's scope is held (an open descriptor stays usable after the scope ends). A chosen folder is walked
// by name for sheet sets.
// Studio Live: `_ccstudio._tcp` through Bonjour (NetService): raw multicast sockets need an entitlement Apple grants case
// by case. The sign-in token is not here: `keyring` keeps it in the Keychain.
//
// NOT BUILT YET: written on Linux, to be built and tried on the owner's Mac (IPAD.md). Refusals start with the shell's
// codes ("not_found: …", "permission_denied: …").

import Foundation
import SwiftRs
import Tauri
import UIKit
import UniformTypeIdentifiers
import WebKit

struct OpenArgs: Decodable { var multiple: Bool? }
struct CreateArgs: Decodable { var name: String?; var mimeType: String? }
struct UriArgs: Decodable { var uri: String; var mode: String? }
struct TreeArgs: Decodable { var tree: String; var rel: [String]?; var create: Bool? }
struct BonjourArgs: Decodable { var name: String; var port: Int?; var app: String?; var ms: Int? }

class CadcraftPlugin: Plugin, UIDocumentPickerDelegate, NetServiceDelegate, NetServiceBrowserDelegate {
  private var pending: ((Result<[URL], Error>) -> Void)?
  private var published: [String: NetService] = [:]

  // ---- documents ----------------------------------------------------------------------------------------------------

  @objc public func openDocuments(_ invoke: Invoke) throws {
    let args = try invoke.parseArgs(OpenArgs.self)
    pick(invoke, UIDocumentPickerViewController(forOpeningContentTypes: [UTType.item], asCopy: false), multiple: args.multiple ?? false) { urls in
      invoke.resolve(["docs": urls.compactMap { self.describe($0, keep: true) }])
    }
  }

  @objc public func createDocument(_ invoke: Invoke) throws {
    let args = try invoke.parseArgs(CreateArgs.self)
    // Save as: an empty file of that name, moved where the person chooses; the picker answers with its new place.
    let name = (args.name?.isEmpty == false ? args.name! : "Untitled").replacingOccurrences(of: "/", with: "_")
    let dir = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString, isDirectory: true)
    try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
    let tmp = dir.appendingPathComponent(name)
    FileManager.default.createFile(atPath: tmp.path, contents: Data())
    pick(invoke, UIDocumentPickerViewController(forExporting: [tmp], asCopy: false), multiple: false) { urls in
      invoke.resolve(["doc": urls.first.flatMap { self.describe($0, keep: true) } as Any])
    }
  }

  @objc public func openTree(_ invoke: Invoke) throws {
    pick(invoke, UIDocumentPickerViewController(forOpeningContentTypes: [UTType.folder], asCopy: false), multiple: false) { urls in
      guard let u = urls.first, var d = self.describe(u, keep: true) else { invoke.resolve(["doc": NSNull()]); return }
      d["dir"] = true
      invoke.resolve(["doc": d])
    }
  }

  /// The file behind a document, opened for Rust: r (read) or wt (write, truncated first).
  @objc public func openFd(_ invoke: Invoke) throws {
    let args = try invoke.parseArgs(UriArgs.self)
    guard let url = resolve(args.uri) else { invoke.reject("not_found: the document is no longer there"); return }
    let scoped = url.startAccessingSecurityScopedResource()
    defer { if scoped { url.stopAccessingSecurityScopedResource() } }
    let write = args.mode == "wt" || args.mode == "w"
    let fd = write ? open(url.path, O_WRONLY | O_CREAT | O_TRUNC, 0o644) : open(url.path, O_RDONLY)
    if fd < 0 {
      let e = errno
      invoke.reject((e == ENOENT ? "not_found: " : e == EACCES || e == EPERM ? "permission_denied: " : "error: ") + String(cString: strerror(e)))
      return
    }
    invoke.resolve(["fd": Int(fd)])
  }

  @objc public func stat(_ invoke: Invoke) throws {
    let args = try invoke.parseArgs(UriArgs.self)
    guard let url = resolve(args.uri) else { invoke.resolve(["doc": NSNull()]); return }
    invoke.resolve(["doc": describe(url, keep: false) as Any])
  }

  @objc public func children(_ invoke: Invoke) throws {
    let args = try invoke.parseArgs(TreeArgs.self)
    guard let tree = resolve(args.tree) else { invoke.reject("not_found: the folder is no longer there"); return }
    let scoped = tree.startAccessingSecurityScopedResource()
    defer { if scoped { tree.stopAccessingSecurityScopedResource() } }
    let dir = (args.rel ?? []).reduce(tree) { $0.appendingPathComponent($1) }
    do {
      let items = try FileManager.default.contentsOfDirectory(at: dir, includingPropertiesForKeys: [.isDirectoryKey, .fileSizeKey, .contentModificationDateKey])
      invoke.resolve(["docs": items.compactMap { self.describe($0, keep: false) }])
    } catch {
      invoke.reject("not_found: \(error.localizedDescription)")
    }
  }

  @objc public func find(_ invoke: Invoke) throws {
    let args = try invoke.parseArgs(TreeArgs.self)
    let rel = args.rel ?? []
    guard let tree = resolve(args.tree), !rel.isEmpty else { invoke.reject("not_found: no such file"); return }
    let scoped = tree.startAccessingSecurityScopedResource()
    defer { if scoped { tree.stopAccessingSecurityScopedResource() } }
    let url = rel.reduce(tree) { $0.appendingPathComponent($1) }
    let fm = FileManager.default
    var isDir: ObjCBool = false
    if !fm.fileExists(atPath: url.path, isDirectory: &isDir) {
      if args.create ?? false {
        try? fm.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        fm.createFile(atPath: url.path, contents: Data())
      } else {
        invoke.resolve(["doc": NSNull()])
        return
      }
    } else if isDir.boolValue {
      invoke.resolve(["doc": NSNull()])
      return
    }
    // Inside the folder's scope a bookmark of the file can be made, and opened later on its own.
    invoke.resolve(["doc": describe(url, keep: true) as Any])
  }

  @objc public func release(_ invoke: Invoke) throws {
    invoke.resolve()   // a bookmark holds nothing to give back
  }

  /// The device's model for the sign-in list ("iPad"): never the person's own name for it.
  @objc public func deviceName(_ invoke: Invoke) throws {
    invoke.resolve(["name": UIDevice.current.model])
  }

  // ---- Studio Live: Bonjour ---------------------------------------------------------------------------------------

  @objc public func bonjourAdvertise(_ invoke: Invoke) throws {
    let args = try invoke.parseArgs(BonjourArgs.self)
    DispatchQueue.main.async {
      self.published[args.name]?.stop()
      let s = NetService(domain: "local.", type: "_ccstudio._tcp.", name: args.name, port: Int32(args.port ?? 0))
      s.setTXTRecord(NetService.data(fromTXTRecord: ["app": (args.app ?? "").data(using: .utf8)!]))
      s.publish()
      self.published[args.name] = s
      invoke.resolve()
    }
  }

  @objc public func bonjourStop(_ invoke: Invoke) throws {
    let args = try invoke.parseArgs(BonjourArgs.self)
    DispatchQueue.main.async {
      self.published.removeValue(forKey: args.name)?.stop()
      invoke.resolve()
    }
  }

  private var browser: NetServiceBrowser?
  private var looking: (name: String, invoke: Invoke, service: NetService?)?

  @objc public func bonjourBrowse(_ invoke: Invoke) throws {
    let args = try invoke.parseArgs(BonjourArgs.self)
    DispatchQueue.main.async {
      self.finishBrowse([])
      self.looking = (args.name, invoke, nil)
      let b = NetServiceBrowser()
      b.delegate = self
      self.browser = b
      b.searchForServices(ofType: "_ccstudio._tcp.", inDomain: "local.")
      DispatchQueue.main.asyncAfter(deadline: .now() + .milliseconds(min(max(args.ms ?? 1500, 100), 5000))) { self.finishBrowse([]) }
    }
  }

  func netServiceBrowser(_ browser: NetServiceBrowser, didFind service: NetService, moreComing: Bool) {
    guard let l = looking, l.service == nil, service.name.caseInsensitiveCompare(l.name) == .orderedSame else { return }
    looking?.service = service
    service.delegate = self
    service.resolve(withTimeout: 4)
  }

  func netServiceDidResolveAddress(_ sender: NetService) {
    let app = sender.txtRecordData().flatMap { NetService.dictionary(fromTXTRecord: $0)["app"] }.flatMap { String(data: $0, encoding: .utf8) } ?? ""
    let found: [[String: Any]] = (sender.addresses ?? []).compactMap { data in
      data.withUnsafeBytes { (p: UnsafeRawBufferPointer) -> [String: Any]? in
        guard let sa = p.baseAddress?.assumingMemoryBound(to: sockaddr.self) else { return nil }
        var host = [CChar](repeating: 0, count: Int(NI_MAXHOST))
        guard getnameinfo(sa, socklen_t(data.count), &host, socklen_t(host.count), nil, 0, NI_NUMERICHOST) == 0 else { return nil }
        let addr = String(cString: host)
        if addr.hasPrefix("fe80") { return nil }   // link-local needs a zone the page can't give
        return ["addr": addr, "port": sender.port, "app": app]
      }
    }
    finishBrowse(found)
  }

  private func finishBrowse(_ found: [[String: Any]]) {
    guard let l = looking else { return }
    looking = nil
    browser?.stop()
    browser = nil
    l.service?.stop()
    l.invoke.resolve(["found": found])
  }

  // ---- helpers ----------------------------------------------------------------------------------------------------

  private func pick(_ invoke: Invoke, _ picker: UIDocumentPickerViewController, multiple: Bool, _ done: @escaping ([URL]) -> Void) {
    DispatchQueue.main.async {
      self.pending = { r in
        switch r {
        case .success(let urls): done(urls)
        case .failure(let e): invoke.reject("error: \(e.localizedDescription)")
        }
      }
      picker.delegate = self
      picker.allowsMultipleSelection = multiple
      picker.modalPresentationStyle = .formSheet
      self.manager.viewController?.present(picker, animated: true, completion: nil)
    }
  }

  public func documentPicker(_ controller: UIDocumentPickerViewController, didPickDocumentsAt urls: [URL]) {
    pending?(.success(urls))
    pending = nil
  }

  public func documentPickerWasCancelled(_ controller: UIDocumentPickerViewController) {
    pending?(.success([]))
    pending = nil
  }

  /// {uri (a bookmark, when keep), name, size, mtime, dir} — read inside the document's scope.
  private func describe(_ url: URL, keep: Bool) -> [String: Any]? {
    let scoped = url.startAccessingSecurityScopedResource()
    defer { if scoped { url.stopAccessingSecurityScopedResource() } }
    var d: [String: Any] = ["name": url.lastPathComponent]
    let v = try? url.resourceValues(forKeys: [.isDirectoryKey, .fileSizeKey, .contentModificationDateKey])
    d["dir"] = v?.isDirectory ?? false
    if let s = v?.fileSize { d["size"] = s }
    if let m = v?.contentModificationDate { d["mtime"] = m.timeIntervalSince1970 * 1000 }
    if keep {
      guard let b = try? url.bookmarkData(options: [], includingResourceValuesForKeys: nil, relativeTo: nil) else { return nil }
      d["uri"] = b.base64EncodedString()
    } else {
      d["uri"] = (try? url.bookmarkData(options: [], includingResourceValuesForKeys: nil, relativeTo: nil))?.base64EncodedString() ?? ""
    }
    return d
  }

  private func resolve(_ uri: String) -> URL? {
    guard let data = Data(base64Encoded: uri) else { return nil }
    var stale = false
    return try? URL(resolvingBookmarkData: data, options: [], relativeTo: nil, bookmarkDataIsStale: &stale)
  }
}

@_cdecl("init_plugin_cadcraft")
public func initPlugin() -> Plugin {
  return CadcraftPlugin()
}
