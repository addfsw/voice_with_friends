package io.github.addfsw.poigrulkiny

import android.Manifest
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Intent
import android.content.pm.PackageManager
import android.content.pm.ServiceInfo
import android.net.wifi.WifiManager
import android.os.Build
import android.os.IBinder
import android.os.PowerManager
import androidx.core.app.NotificationCompat
import androidx.core.content.ContextCompat

/// Пока идёт звонок, программа работает «на переднем плане» с уведомлением:
/// так Android не отключает микрофон и сеть, когда экран погас или открыта
/// другая программа (игра). Процессор и Wi-Fi тоже не засыпают, а Wi-Fi
/// принимает широковещательные пакеты — по ним друзья в той же сети находят
/// комнату, открытую на телефоне.
class CallService : Service() {
  companion object {
    const val EXTRA_ROOM = "room"
    /// Кнопка микрофона в уведомлении: MainActivity ловит эту рассылку и
    /// переключает микрофон в интерфейсе.
    const val ACTION_TOGGLE_MIC = "io.github.addfsw.poigrulkiny.TOGGLE_MIC"
    private const val CHANNEL = "call"
    private const val NOTIFICATION_ID = 1

    @Volatile private var running: CallService? = null
    /// Выключен ли микрофон — для текста и кнопки в уведомлении.
    @Volatile private var muted = false

    /// Микрофон выключили или включили — обновить уведомление. Сервис
    /// заново не запускается: из фона Android этого не позволяет.
    fun setMuted(value: Boolean) {
      muted = value
      val service = running ?: return
      service.getSystemService(NotificationManager::class.java)
        .notify(NOTIFICATION_ID, service.notification())
    }
  }

  private var wakeLock: PowerManager.WakeLock? = null
  private var wifiLock: WifiManager.WifiLock? = null
  private var multicastLock: WifiManager.MulticastLock? = null
  private var room = "Голосовая комната"

  override fun onBind(intent: Intent?): IBinder? = null

  override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
    val manager = getSystemService(NotificationManager::class.java)
    if (Build.VERSION.SDK_INT >= 26) {
      manager.createNotificationChannel(
        NotificationChannel(CHANNEL, "Голосовая комната", NotificationManager.IMPORTANCE_LOW)
          .apply { setShowBadge(false) }
      )
    }
    intent?.getStringExtra(EXTRA_ROOM)?.takeIf { it.isNotBlank() }?.let { room = it }
    val notification = notification()

    // Тип «микрофон» можно заявить, только если разрешение уже дано.
    val micAllowed = ContextCompat.checkSelfPermission(this, Manifest.permission.RECORD_AUDIO) ==
      PackageManager.PERMISSION_GRANTED
    try {
      if (Build.VERSION.SDK_INT >= 30) {
        var type = ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PLAYBACK
        if (micAllowed) type = type or ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE
        startForeground(NOTIFICATION_ID, notification, type)
      } else if (Build.VERSION.SDK_INT >= 29) {
        startForeground(
          NOTIFICATION_ID,
          notification,
          ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PLAYBACK
        )
      } else {
        startForeground(NOTIFICATION_ID, notification)
      }
    } catch (e: Exception) {
      // Не дали запуститься в фоне — звонок всё равно работает, пока
      // программа открыта.
      stopSelf()
      return START_NOT_STICKY
    }
    running = this

    if (wakeLock == null) {
      wakeLock = getSystemService(PowerManager::class.java)
        .newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "poigrulkiny:call")
        .apply { acquire() }
    }
    if (wifiLock == null) {
      val mode = if (Build.VERSION.SDK_INT >= 29) {
        WifiManager.WIFI_MODE_FULL_LOW_LATENCY
      } else {
        @Suppress("DEPRECATION")
        WifiManager.WIFI_MODE_FULL_HIGH_PERF
      }
      wifiLock = applicationContext.getSystemService(WifiManager::class.java)
        ?.createWifiLock(mode, "poigrulkiny:call")
        ?.apply { acquire() }
    }
    if (multicastLock == null) {
      multicastLock = applicationContext.getSystemService(WifiManager::class.java)
        ?.createMulticastLock("poigrulkiny:discovery")
        ?.apply {
          setReferenceCounted(false)
          acquire()
        }
    }
    return START_NOT_STICKY
  }

  private fun notification(): Notification {
    val open = PendingIntent.getActivity(
      this,
      0,
      Intent(this, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP),
      PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT
    )
    // «Выйти» открывает программу: хосту она сначала покажет вопрос, ведь
    // закрытие комнаты отключит всех.
    val leave = PendingIntent.getActivity(
      this,
      1,
      Intent(this, MainActivity::class.java)
        .addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP)
        .putExtra(MainActivity.EXTRA_LEAVE, true),
      PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT
    )
    // Микрофон выключается прямо из уведомления, не открывая программу
    // (например, посреди игры).
    val toggleMic = PendingIntent.getBroadcast(
      this,
      2,
      Intent(ACTION_TOGGLE_MIC).setPackage(packageName),
      PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT
    )
    // Одна тихая строка: название комнаты и кнопки микрофона и «Выйти», без
    // звука и времени.
    return NotificationCompat.Builder(this, CHANNEL)
      .setSmallIcon(R.drawable.ic_notification)
      .setColor(0xFF7C6CFF.toInt())
      .setContentTitle(room)
      .setContentText(if (muted) "Микрофон выключен" else "Вы в голосовой комнате")
      .setContentIntent(open)
      .addAction(0, if (muted) "Включить микрофон" else "Выключить микрофон", toggleMic)
      .addAction(0, "Выйти", leave)
      .setOngoing(true)
      .setSilent(true)
      .setShowWhen(false)
      .setPriority(NotificationCompat.PRIORITY_LOW)
      .setForegroundServiceBehavior(NotificationCompat.FOREGROUND_SERVICE_IMMEDIATE)
      .setCategory(NotificationCompat.CATEGORY_CALL)
      .build()
  }

  override fun onDestroy() {
    running = null
    wakeLock?.takeIf { it.isHeld }?.release()
    wifiLock?.takeIf { it.isHeld }?.release()
    multicastLock?.takeIf { it.isHeld }?.release()
    wakeLock = null
    wifiLock = null
    multicastLock = null
    super.onDestroy()
  }
}
