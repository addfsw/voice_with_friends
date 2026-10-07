package io.github.addfsw.poigrulkiny

import android.Manifest
import android.content.BroadcastReceiver
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.ActivityInfo
import android.content.pm.PackageManager
import android.graphics.Color
import android.net.ConnectivityManager
import android.net.NetworkCapabilities
import android.os.Build
import android.os.Bundle
import android.webkit.JavascriptInterface
import android.webkit.WebView
import java.io.File
import androidx.activity.OnBackPressedCallback
import androidx.activity.enableEdgeToEdge
import androidx.core.app.ActivityCompat
import androidx.core.content.ContextCompat
import androidx.core.content.FileProvider
import androidx.core.view.ViewCompat
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat
import androidx.lifecycle.Lifecycle

class MainActivity : TauriActivity() {
  companion object {
    /// Нажали уведомление чата: открыть вкладку «Чат».
    const val EXTRA_OPEN_CHAT = "open_chat"
    /// Нажали «Выйти» в уведомлении звонка.
    const val EXTRA_LEAVE = "leave"
  }

  private val callAudio by lazy { CallAudio(this) }
  private val chatNotifier by lazy { ChatNotifier(this) }
  private var webView: WebView? = null
  /// Приглашение, с которым программу открыли (ссылка или «Поделиться»),
  /// пока интерфейс его не забрал (takeInvite).
  @Volatile private var pendingInvite: String? = null

  /// Кнопка микрофона в уведомлении звонка (CallService). Интерфейс
  /// переключает микрофон и сам сообщает новое состояние через setMuted.
  private val micToggle = object : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
      webView?.evaluateJavascript("window.onAndroidToggleMic && window.onAndroidToggleMic()", null)
    }
  }

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

    ContextCompat.registerReceiver(
      this,
      micToggle,
      IntentFilter(CallService.ACTION_TOGGLE_MIC),
      ContextCompat.RECEIVER_NOT_EXPORTED
    )
    pendingInvite = inviteText(intent)
    requestPermissions()
  }

  /// Текст с кодом приглашения: ссылка poigrulkiny://join/poi-… или текст,
  /// которым поделились из мессенджера. Код в нём ищет интерфейс.
  private fun inviteText(intent: Intent?): String? {
    if (intent == null) return null
    val text = when (intent.action) {
      Intent.ACTION_VIEW -> intent.dataString
      Intent.ACTION_SEND -> intent.getStringExtra(Intent.EXTRA_TEXT)
      else -> null
    }
    return text?.takeIf { it.contains("poi-") }
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

  /// Программа на экране — сообщения чата видны в ней самой.
  override fun onResume() {
    super.onResume()
    chatNotifier.clear()
  }

  /// Нажатие на уведомление, когда программа уже запущена.
  override fun onNewIntent(intent: Intent) {
    super.onNewIntent(intent)
    inviteText(intent)?.let { text ->
      pendingInvite = text
      webView?.evaluateJavascript("window.onAndroidInvite && window.onAndroidInvite()", null)
      return
    }
    val script = when {
      intent.getBooleanExtra(EXTRA_LEAVE, false) -> "window.onAndroidLeave && window.onAndroidLeave()"
      intent.getBooleanExtra(EXTRA_OPEN_CHAT, false) -> "window.onAndroidOpenChat && window.onAndroidOpenChat()"
      else -> return
    }
    webView?.evaluateJavascript(script, null)
  }

  override fun onDestroy() {
    unregisterReceiver(micToggle)
    callAudio.stop()
    chatNotifier.clear()
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
      // Состояние микрофона интерфейс сообщит сразу после (setMuted).
      CallService.setMuted(false)
      val intent = Intent(this@MainActivity, CallService::class.java)
        .putExtra(CallService.EXTRA_ROOM, room)
      ContextCompat.startForegroundService(this@MainActivity, intent)
    }

    /// Микрофон выключили или включили — обновить уведомление звонка.
    @JavascriptInterface
    fun setMuted(muted: Boolean) {
      CallService.setMuted(muted)
    }

    @JavascriptInterface
    fun callEnded() {
      stopService(Intent(this@MainActivity, CallService::class.java))
      runOnUiThread {
        callAudio.stop()
        chatNotifier.clear()
      }
    }

    /// Новое сообщение чата от собеседника. Уведомление — только если
    /// программа не на экране.
    @JavascriptInterface
    fun chatMessage(room: String, name: String, text: String, timeMs: Double) {
      runOnUiThread {
        if (!lifecycle.currentState.isAtLeast(Lifecycle.State.STARTED)) {
          chatNotifier.show(room, name, text, timeMs.toLong())
        }
      }
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

    /// Забрать приглашение, с которым открыли программу (или null).
    @JavascriptInterface
    fun takeInvite(): String? {
      val text = pendingInvite
      pendingInvite = null
      return text
    }

    /// «Поделиться» приглашением: системное меню (мессенджеры и т.п.).
    @JavascriptInterface
    fun share(text: String) {
      runOnUiThread {
        val send = Intent(Intent.ACTION_SEND)
          .setType("text/plain")
          .putExtra(Intent.EXTRA_TEXT, text)
        // Себя в списке не показываем: «Поигрулькины» принимают приглашения.
        val chooser = Intent.createChooser(send, "Пригласить в комнату")
          .putExtra(
            Intent.EXTRA_EXCLUDE_COMPONENTS,
            arrayOf(ComponentName(this@MainActivity, MainActivity::class.java))
          )
        startActivity(chooser)
      }
    }

    /// «Отправить программу другу»: установленный APK — в системное меню
    /// (Telegram и т.п.). Подписан тем же ключом, поэтому у друга встаёт
    /// поверх старой версии.
    @JavascriptInterface
    fun shareApp() {
      Thread {
        val uri = try {
          val dir = File(cacheDir, "apk").apply { mkdirs() }
          val apk = File(dir, "Poigrulkiny.apk")
          File(applicationInfo.sourceDir).copyTo(apk, overwrite = true)
          FileProvider.getUriForFile(this@MainActivity, "$packageName.fileprovider", apk)
        } catch (e: Exception) {
          return@Thread
        }
        runOnUiThread {
          val send = Intent(Intent.ACTION_SEND)
            .setType("application/vnd.android.package-archive")
            .putExtra(Intent.EXTRA_STREAM, uri)
            .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
          startActivity(Intent.createChooser(send, "Отправить программу"))
        }
      }.start()
    }

    @JavascriptInterface
    fun setTheme(dark: Boolean) {
      runOnUiThread { setDarkTheme(dark) }
    }
  }
}
