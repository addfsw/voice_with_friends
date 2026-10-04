package io.github.addfsw.poigrulkiny

import android.Manifest
import android.content.Intent
import android.content.pm.PackageManager
import android.graphics.Color
import android.os.Build
import android.os.Bundle
import android.webkit.JavascriptInterface
import android.webkit.WebView
import androidx.activity.OnBackPressedCallback
import androidx.activity.enableEdgeToEdge
import androidx.core.app.ActivityCompat
import androidx.core.content.ContextCompat
import androidx.core.view.ViewCompat
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat

class MainActivity : TauriActivity() {
  override fun onCreate(savedInstanceState: Bundle?) {
    enableEdgeToEdge()
    super.onCreate(savedInstanceState)

    // Интерфейс не залезает под строку состояния и панель навигации, а поле
    // чата — под клавиатуру.
    val content = findViewById<android.view.View>(android.R.id.content)
    ViewCompat.setOnApplyWindowInsetsListener(content) { view, insets ->
      val bars = insets.getInsets(
        WindowInsetsCompat.Type.systemBars() or
          WindowInsetsCompat.Type.displayCutout() or
          WindowInsetsCompat.Type.ime()
      )
      view.setPadding(bars.left, bars.top, bars.right, bars.bottom)
      WindowInsetsCompat.CONSUMED
    }
    setDarkTheme(true)

    // «Назад» сворачивает программу, а не закрывает: иначе при следующем
    // открытии окно загрузилось бы заново и звонок оборвался.
    onBackPressedDispatcher.addCallback(this, object : OnBackPressedCallback(true) {
      override fun handleOnBackPressed() {
        moveTaskToBack(true)
      }
    })

    requestPermissions()
  }

  override fun onWebViewCreate(webView: WebView) {
    webView.addJavascriptInterface(AndroidBridge(), "PoigrulkinyAndroid")
  }

  /// Микрофон нужен для разговора, уведомления — чтобы Android показал, что
  /// звонок идёт в фоне (без этого он отключит микрофон).
  private fun requestPermissions() {
    val wanted = mutableListOf(Manifest.permission.RECORD_AUDIO)
    if (Build.VERSION.SDK_INT >= 33) wanted += Manifest.permission.POST_NOTIFICATIONS
    val missing = wanted.filter {
      ContextCompat.checkSelfPermission(this, it) != PackageManager.PERMISSION_GRANTED
    }
    if (missing.isNotEmpty()) {
      ActivityCompat.requestPermissions(this, missing.toTypedArray(), 1)
    }
  }

  /// Цвет подложки под системными панелями — как фон интерфейса.
  fun setDarkTheme(dark: Boolean) {
    window.decorView.setBackgroundColor(if (dark) Color.rgb(15, 17, 21) else Color.rgb(243, 244, 247))
    val controller = WindowCompat.getInsetsController(window, window.decorView)
    controller.isAppearanceLightStatusBars = !dark
    controller.isAppearanceLightNavigationBars = !dark
  }

  /// Вызовы из интерфейса (app/ui/app.js): `window.PoigrulkinyAndroid`.
  inner class AndroidBridge {
    /// Начался звонок: сервис с уведомлением держит микрофон и сеть в фоне.
    @JavascriptInterface
    fun callStarted(room: String) {
      val intent = Intent(this@MainActivity, CallService::class.java)
        .putExtra(CallService.EXTRA_ROOM, room)
      ContextCompat.startForegroundService(this@MainActivity, intent)
    }

    @JavascriptInterface
    fun callEnded() {
      stopService(Intent(this@MainActivity, CallService::class.java))
    }

    @JavascriptInterface
    fun setTheme(dark: Boolean) {
      runOnUiThread { setDarkTheme(dark) }
    }
  }
}
