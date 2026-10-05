package io.github.addfsw.poigrulkiny

import android.Manifest
import android.content.Intent
import android.content.pm.ActivityInfo
import android.content.pm.PackageManager
import android.graphics.Color
import android.net.ConnectivityManager
import android.net.NetworkCapabilities
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
import androidx.core.view.WindowInsetsControllerCompat

class MainActivity : TauriActivity() {
  private val callAudio by lazy { CallAudio(this) }
  private var webView: WebView? = null

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

    // «Назад» сначала закрывает то, что открыто поверх комнаты (просмотр
    // экрана, см. window.onAndroidBack в app.js), а иначе сворачивает
    // программу, а не закрывает: иначе при следующем открытии окно
    // загрузилось бы заново и звонок оборвался.
    onBackPressedDispatcher.addCallback(this, object : OnBackPressedCallback(true) {
      override fun handleOnBackPressed() {
        val view = webView
        if (view == null) {
          moveTaskToBack(true)
          return
        }
        view.evaluateJavascript("Boolean(window.onAndroidBack && window.onAndroidBack())") {
          if (it != "true") moveTaskToBack(true)
        }
      }
    })

    requestPermissions()
  }

  override fun onWebViewCreate(webView: WebView) {
    this.webView = webView
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

  override fun onDestroy() {
    callAudio.stop()
    super.onDestroy()
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
    /// Перед подключением: режим «разговор» (эхоподавление) и куда идёт звук.
    /// Включаем до того, как откроется звук, — иначе Android переключит его
    /// на ходу.
    @JavascriptInterface
    fun prepareCall(route: String) {
      runOnUiThread {
        callAudio.setRoute(route)
        callAudio.start()
      }
    }

    /// Куда идёт звук: "auto", "speaker" или "earpiece" (см. CallAudio).
    @JavascriptInterface
    fun setAudioRoute(route: String) {
      runOnUiThread { callAudio.setRoute(route) }
    }

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
      runOnUiThread { callAudio.stop() }
    }

    /// Просмотр чужого экрана: на весь экран, без строки состояния и панели
    /// навигации (их можно вытянуть жестом).
    @JavascriptInterface
    fun setImmersive(on: Boolean) {
      runOnUiThread {
        val controller = WindowCompat.getInsetsController(window, window.decorView)
        if (on) {
          controller.systemBarsBehavior =
            WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
          controller.hide(WindowInsetsCompat.Type.systemBars())
        } else {
          controller.show(WindowInsetsCompat.Type.systemBars())
        }
      }
    }

    /// «Повернуть» в просмотре экрана: альбомная ориентация, даже если
    /// автоповорот выключен; false — снова как настроено в телефоне.
    @JavascriptInterface
    fun setLandscape(on: Boolean) {
      runOnUiThread {
        requestedOrientation = if (on) {
          ActivityInfo.SCREEN_ORIENTATION_SENSOR_LANDSCAPE
        } else {
          ActivityInfo.SCREEN_ORIENTATION_UNSPECIFIED
        }
      }
    }

    /// Какая сейчас сеть: "wifi" (или проводная), "cellular" или "none".
    /// Своя комната на телефоне доступна друзьям только из Wi-Fi.
    @JavascriptInterface
    fun networkType(): String {
      val connectivity = getSystemService(ConnectivityManager::class.java)
      val caps = connectivity.getNetworkCapabilities(connectivity.activeNetwork) ?: return "none"
      return when {
        caps.hasTransport(NetworkCapabilities.TRANSPORT_WIFI) ||
          caps.hasTransport(NetworkCapabilities.TRANSPORT_ETHERNET) -> "wifi"
        caps.hasTransport(NetworkCapabilities.TRANSPORT_CELLULAR) -> "cellular"
        else -> "other"
      }
    }

    @JavascriptInterface
    fun setTheme(dark: Boolean) {
      runOnUiThread { setDarkTheme(dark) }
    }
  }
}
