// SPDX-License-Identifier: Apache-2.0
// CadcraftPlugin.kt — the Android half of the shell's own mobile plugin (desktop-shell/src-tauri/src/mobile.rs; docs/0.5-PART3.md
// TB.1). Rust calls these commands by name; every answer is JSON.
//
// Documents (the Storage Access Framework): the person picks with the system's own pickers; each pick keeps a lasting
// permission (takePersistableUriPermission), so a recent file reopens after a restart. Rust reads and writes through file
// descriptors this plugin opens and hands over (detachFd). A chosen folder ("tree") is walked by name for sheet sets.
// The sign-in token: sealed with an AES key that lives in the Android Keystore and never leaves it; the sealed token sits in
// the app's private preferences (not backed up: android:allowBackup="false").
// Refusals start with the shell's codes: "not_found: …", "permission_denied: …".
package com.getcadcraft.studio

import android.app.Activity
import android.content.Context
import android.content.Intent
import android.database.Cursor
import android.net.Uri
import android.net.wifi.WifiManager
import android.os.Build
import android.provider.DocumentsContract
import android.provider.OpenableColumns
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.Base64
import androidx.activity.result.ActivityResult
import app.tauri.annotation.ActivityCallback
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSArray
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import java.io.FileNotFoundException
import org.json.JSONObject
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

@InvokeArg
class OpenArgs {
  var multiple: Boolean = false
}

@InvokeArg
class CreateArgs {
  var name: String = "Untitled"
  var mimeType: String = "application/octet-stream"
}

@InvokeArg
class UriArgs {
  var uri: String = ""
  var mode: String = "r"
}

@InvokeArg
class TreeArgs {
  var tree: String = ""
  var rel: Array<String> = arrayOf()
  var create: Boolean = false
}

@InvokeArg
class SecretArgs {
  var key: String = ""
  var value: String? = null
}

@TauriPlugin
class CadcraftPlugin(private val activity: Activity) : Plugin(activity) {
  private val resolver get() = activity.contentResolver
  private val rw = Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION

  // ---- documents --------------------------------------------------------------------------------------------------

  @Command
  fun openDocuments(invoke: Invoke) {
    val a = invoke.parseArgs(OpenArgs::class.java)
    val i = Intent(Intent.ACTION_OPEN_DOCUMENT).apply {
      addCategory(Intent.CATEGORY_OPENABLE)
      type = "*/*"
      putExtra(Intent.EXTRA_ALLOW_MULTIPLE, a.multiple)
      addFlags(rw or Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION)
    }
    startActivityForResult(invoke, i, "openDocumentsResult")
  }

  @ActivityCallback
  fun openDocumentsResult(invoke: Invoke, result: ActivityResult) {
    val docs = JSArray()
    val data = result.data
    if (result.resultCode == Activity.RESULT_OK && data != null) {
      val uris = mutableListOf<Uri>()
      val clip = data.clipData
      if (clip != null) for (k in 0 until clip.itemCount) uris.add(clip.getItemAt(k).uri) else data.data?.let { uris.add(it) }
      for (u in uris) {
        keep(u)
        describe(u)?.let { docs.put(it) }
      }
    }
    invoke.resolve(JSObject().put("docs", docs))
  }

  @Command
  fun createDocument(invoke: Invoke) {
    val a = invoke.parseArgs(CreateArgs::class.java)
    val i = Intent(Intent.ACTION_CREATE_DOCUMENT).apply {
      addCategory(Intent.CATEGORY_OPENABLE)
      type = a.mimeType
      putExtra(Intent.EXTRA_TITLE, a.name)
      addFlags(rw or Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION)
    }
    startActivityForResult(invoke, i, "createDocumentResult")
  }

  @ActivityCallback
  fun createDocumentResult(invoke: Invoke, result: ActivityResult) {
    val u = if (result.resultCode == Activity.RESULT_OK) result.data?.data else null
    if (u != null) keep(u)
    invoke.resolve(JSObject().put("doc", u?.let { describe(it) }))
  }

  @Command
  fun openTree(invoke: Invoke) {
    val i = Intent(Intent.ACTION_OPEN_DOCUMENT_TREE).apply { addFlags(rw or Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION) }
    startActivityForResult(invoke, i, "openTreeResult")
  }

  @ActivityCallback
  fun openTreeResult(invoke: Invoke, result: ActivityResult) {
    val tree = if (result.resultCode == Activity.RESULT_OK) result.data?.data else null
    if (tree == null) { invoke.resolve(JSObject().put("doc", JSONObject.NULL)); return }
    keep(tree)
    val root = DocumentsContract.buildDocumentUriUsingTree(tree, DocumentsContract.getTreeDocumentId(tree))
    val d = describe(root) ?: JSObject().put("name", "Folder")
    d.put("uri", tree.toString())
    d.put("dir", true)
    invoke.resolve(JSObject().put("doc", d))
  }

  /** The file behind a document, opened for Rust: r (read) or wt (write, truncated first). */
  @Command
  fun openFd(invoke: Invoke) {
    val a = invoke.parseArgs(UriArgs::class.java)
    val mode = if (a.mode == "wt" || a.mode == "w") "wt" else "r"
    try {
      val pfd = resolver.openFileDescriptor(Uri.parse(a.uri), mode) ?: throw FileNotFoundException(a.uri)
      invoke.resolve(JSObject().put("fd", pfd.detachFd()))
    } catch (e: FileNotFoundException) {
      invoke.reject("not_found: ${e.message}")
    } catch (e: SecurityException) {
      invoke.reject("permission_denied: ${e.message}")
    } catch (e: Exception) {
      invoke.reject("error: ${e.message}")
    }
  }

  @Command
  fun stat(invoke: Invoke) {
    val a = invoke.parseArgs(UriArgs::class.java)
    val u = Uri.parse(a.uri)
    val d = try {
      if (DocumentsContract.isTreeUri(u) && !isDocumentInTree(u)) describe(DocumentsContract.buildDocumentUriUsingTree(u, DocumentsContract.getTreeDocumentId(u)))
      else describe(u)
    } catch (e: Exception) { null }
    invoke.resolve(JSObject().put("doc", d))
  }

  /** A chosen folder's entries, `rel` = the path below it. */
  @Command
  fun children(invoke: Invoke) {
    val a = invoke.parseArgs(TreeArgs::class.java)
    try {
      val tree = Uri.parse(a.tree)
      val dirId = walk(tree, a.rel.toList(), false) ?: throw FileNotFoundException(a.rel.joinToString("/"))
      val out = JSArray()
      listChildren(tree, dirId).forEach { out.put(it.second) }
      invoke.resolve(JSObject().put("docs", out))
    } catch (e: FileNotFoundException) {
      invoke.reject("not_found: ${e.message}")
    } catch (e: SecurityException) {
      invoke.reject("permission_denied: ${e.message}")
    }
  }

  /** A file below a chosen folder by its path; with create, made (folders too) when missing. */
  @Command
  fun find(invoke: Invoke) {
    val a = invoke.parseArgs(TreeArgs::class.java)
    try {
      val tree = Uri.parse(a.tree)
      if (a.rel.isEmpty()) throw FileNotFoundException("no name")
      val parentId = walk(tree, a.rel.dropLast(1), a.create)
      if (parentId == null) { invoke.resolve(JSObject().put("doc", JSONObject.NULL)); return }
      val name = a.rel.last()
      var hit = listChildren(tree, parentId).firstOrNull { it.second.getString("name") == name }?.second
      if (hit == null && a.create) {
        val parent = DocumentsContract.buildDocumentUriUsingTree(tree, parentId)
        val made = DocumentsContract.createDocument(resolver, parent, "application/octet-stream", name)
        hit = made?.let { describe(it) }
      }
      if (hit != null && hit.optBoolean("dir")) hit = null
      invoke.resolve(JSObject().put("doc", hit))
    } catch (e: FileNotFoundException) {
      invoke.reject("not_found: ${e.message}")
    } catch (e: SecurityException) {
      invoke.reject("permission_denied: ${e.message}")
    }
  }

  @Command
  fun release(invoke: Invoke) {
    val a = invoke.parseArgs(UriArgs::class.java)
    try { resolver.releasePersistableUriPermission(Uri.parse(a.uri), rw) } catch (e: Exception) { /* already gone */ }
    invoke.resolve()
  }

  /** The device's model for the sign-in list ("Pixel Tablet"): never the person's own name for it. */
  @Command
  fun deviceName(invoke: Invoke) {
    invoke.resolve(JSObject().put("name", Build.MODEL ?: ""))
  }

  // ---- Studio Live on the LAN (TB.5) ------------------------------------------------------------------------------

  private var multicastLock: WifiManager.MulticastLock? = null
  private var multicastWanted = false

  /** mDNS (_ccstudio._tcp) needs Wi-Fi multicast: held while Studio is on screen, once Live asked for it. */
  @Command
  fun multicast(invoke: Invoke) {
    multicastWanted = invoke.getArgs().optBoolean("on", false)
    holdMulticast(multicastWanted)
    invoke.resolve()
  }

  override fun onResume() { if (multicastWanted) holdMulticast(true) }

  override fun onPause() { holdMulticast(false) }

  private fun holdMulticast(on: Boolean) {
    try {
      if (on) {
        val wm = activity.applicationContext.getSystemService(Context.WIFI_SERVICE) as WifiManager
        val l = multicastLock ?: wm.createMulticastLock("cadcraft-live").also { it.setReferenceCounted(false); multicastLock = it }
        if (!l.isHeld) l.acquire()
      } else {
        multicastLock?.let { if (it.isHeld) it.release() }
      }
    } catch (e: Exception) { /* no Wi-Fi: Live says what it can't find */ }
  }

  // ---- the sign-in token ------------------------------------------------------------------------------------------

  @Command
  fun secretGet(invoke: Invoke) {
    val a = invoke.parseArgs(SecretArgs::class.java)
    val sealed = prefs().getString(a.key, null)
    val value = sealed?.let {
      try {
        val b = Base64.decode(it, Base64.NO_WRAP)
        val c = Cipher.getInstance("AES/GCM/NoPadding")
        c.init(Cipher.DECRYPT_MODE, key(), GCMParameterSpec(128, b, 0, 12))
        String(c.doFinal(b, 12, b.size - 12), Charsets.UTF_8)
      } catch (e: Exception) {
        prefs().edit().remove(a.key).commit()   // the Keystore key is gone (a restore, a reset): sign in again
        null
      }
    }
    invoke.resolve(JSObject().put("value", value))
  }

  @Command
  fun secretSet(invoke: Invoke) {
    val a = invoke.parseArgs(SecretArgs::class.java)
    try {
      val c = Cipher.getInstance("AES/GCM/NoPadding")
      c.init(Cipher.ENCRYPT_MODE, key())
      val ct = c.doFinal((a.value ?: "").toByteArray(Charsets.UTF_8))
      val out = c.iv + ct
      if (!prefs().edit().putString(a.key, Base64.encodeToString(out, Base64.NO_WRAP)).commit()) throw Exception("not written")
      invoke.resolve()
    } catch (e: Exception) {
      invoke.reject("error: ${e.message}")
    }
  }

  @Command
  fun secretDelete(invoke: Invoke) {
    val a = invoke.parseArgs(SecretArgs::class.java)
    prefs().edit().remove(a.key).commit()
    invoke.resolve()
  }

  // ---- helpers ----------------------------------------------------------------------------------------------------

  private fun prefs() = activity.getSharedPreferences("cadcraft.secrets", Context.MODE_PRIVATE)

  private fun key(): SecretKey {
    val ks = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
    (ks.getKey(ALIAS, null) as? SecretKey)?.let { return it }
    val g = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore")
    g.init(KeyGenParameterSpec.Builder(ALIAS, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
      .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
      .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
      .setKeySize(256)
      .build())
    return g.generateKey()
  }

  /** Keep the permission across restarts: read and write when the provider gives both, else read. */
  private fun keep(u: Uri) {
    try { resolver.takePersistableUriPermission(u, rw) } catch (e: SecurityException) {
      try { resolver.takePersistableUriPermission(u, Intent.FLAG_GRANT_READ_URI_PERMISSION) } catch (e2: SecurityException) { /* this session only */ }
    }
  }

  private fun isDocumentInTree(u: Uri): Boolean = try { DocumentsContract.getDocumentId(u); true } catch (e: Exception) { false }

  private fun row(c: Cursor, uri: Uri): JSObject {
    val o = JSObject().put("uri", uri.toString())
    fun col(n: String) = c.getColumnIndex(n)
    val name = col(OpenableColumns.DISPLAY_NAME).let { if (it >= 0) c.getString(it) else null }
      ?: col(DocumentsContract.Document.COLUMN_DISPLAY_NAME).let { if (it >= 0) c.getString(it) else null }
    o.put("name", name ?: uri.lastPathSegment ?: "file")
    col(OpenableColumns.SIZE).let { if (it >= 0 && !c.isNull(it)) o.put("size", c.getLong(it)) }
    col(DocumentsContract.Document.COLUMN_LAST_MODIFIED).let { if (it >= 0 && !c.isNull(it)) o.put("mtime", c.getLong(it).toDouble()) }
    val mime = col(DocumentsContract.Document.COLUMN_MIME_TYPE).let { if (it >= 0) c.getString(it) else null }
    o.put("dir", mime == DocumentsContract.Document.MIME_TYPE_DIR)
    return o
  }

  private fun describe(u: Uri): JSObject? = try {
    resolver.query(u, null, null, null, null)?.use { c -> if (c.moveToFirst()) row(c, u) else null }
  } catch (e: Exception) { null }

  /** (document id, description) of each entry in a folder of a chosen tree. */
  private fun listChildren(tree: Uri, dirId: String): List<Pair<String, JSObject>> {
    val kids = DocumentsContract.buildChildDocumentsUriUsingTree(tree, dirId)
    val cols = arrayOf(DocumentsContract.Document.COLUMN_DOCUMENT_ID, DocumentsContract.Document.COLUMN_DISPLAY_NAME,
      DocumentsContract.Document.COLUMN_SIZE, DocumentsContract.Document.COLUMN_LAST_MODIFIED, DocumentsContract.Document.COLUMN_MIME_TYPE)
    val out = mutableListOf<Pair<String, JSObject>>()
    resolver.query(kids, cols, null, null, null)?.use { c ->
      while (c.moveToNext()) {
        val id = c.getString(0)
        val o = JSObject()
          .put("uri", DocumentsContract.buildDocumentUriUsingTree(tree, id).toString())
          .put("name", c.getString(1) ?: id)
          .put("dir", c.getString(4) == DocumentsContract.Document.MIME_TYPE_DIR)
        if (!c.isNull(2)) o.put("size", c.getLong(2))
        if (!c.isNull(3)) o.put("mtime", c.getLong(3).toDouble())
        out.add(Pair(id, o))
      }
    }
    return out
  }

  /** The document id of the folder `parts` below the tree's root; with create, missing folders are made. */
  private fun walk(tree: Uri, parts: List<String>, create: Boolean): String? {
    var id = DocumentsContract.getTreeDocumentId(tree)
    for (p in parts) {
      val next = listChildren(tree, id).firstOrNull { it.second.getString("name") == p && it.second.optBoolean("dir") }?.first
      id = next ?: if (create) {
        val made = DocumentsContract.createDocument(resolver, DocumentsContract.buildDocumentUriUsingTree(tree, id), DocumentsContract.Document.MIME_TYPE_DIR, p)
          ?: return null
        DocumentsContract.getDocumentId(made)
      } else return null
    }
    return id
  }

  companion object {
    private const val ALIAS = "cadcraft-signin"
  }
}
