// SPDX-License-Identifier: Apache-2.0
package com.getcadcraft.studio

import android.content.Intent
import android.net.Uri
import android.os.Bundle
import android.view.View
import androidx.activity.enableEdgeToEdge
import androidx.appcompat.app.AlertDialog
import androidx.webkit.WebViewCompat
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat

class MainActivity : TauriActivity() {
  override fun onCreate(savedInstanceState: Bundle?) {
    enableEdgeToEdge()
    super.onCreate(savedInstanceState)
    // TB.4: Studio's page stays clear of the status and navigation bars (and a camera cutout), and the on-screen keyboard
    // makes the web view shorter instead of covering it, so an app's command line sits just above the keys.
    val root = findViewById<View>(android.R.id.content)
    ViewCompat.setOnApplyWindowInsetsListener(root) { v, insets ->
      val bars = insets.getInsets(WindowInsetsCompat.Type.systemBars() or WindowInsetsCompat.Type.displayCutout())
      val ime = insets.getInsets(WindowInsetsCompat.Type.ime())
      v.setPadding(bars.left, bars.top, bars.right, maxOf(bars.bottom, ime.bottom))
      WindowInsetsCompat.CONSUMED
    }
    checkWebView()
  }

  /** The apps need a current web engine. Android System WebView is updated by Google Play; on a tablet that has not
   *  updated it, Studio would show a blank page, so it says why and offers the update instead (TB.2). */
  private fun checkWebView() {
    val pkg = WebViewCompat.getCurrentWebViewPackage(this) ?: return
    val major = pkg.versionName?.substringBefore('.')?.toIntOrNull() ?: return
    if (major >= MIN_WEBVIEW) return
    AlertDialog.Builder(this)
      .setTitle(R.string.webview_old_title)
      .setMessage(getString(R.string.webview_old_body, major, MIN_WEBVIEW))
      .setPositiveButton(R.string.webview_old_update) { _, _ ->
        val play = Intent(Intent.ACTION_VIEW, Uri.parse("market://details?id=${pkg.packageName}"))
        try { startActivity(play) } catch (e: Exception) {
          startActivity(Intent(Intent.ACTION_VIEW, Uri.parse("https://play.google.com/store/apps/details?id=${pkg.packageName}")))
        }
      }
      .setNegativeButton(R.string.webview_old_later, null)
      .show()
  }

  companion object {
    /** Chromium 105, a little above what the code needs today: structuredClone (98), the inert attribute (102, the views). */
    const val MIN_WEBVIEW = 105
  }
}
