package io.github.addfsw.poigrulkiny

import android.Manifest
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
    private const val CHANNEL = "call"
    private const val NOTIFICATION_ID = 1
  }

  private var wakeLock: PowerManager.WakeLock? = null
  private var wifiLock: WifiManager.WifiLock? = null
  private var multicastLock: WifiManager.MulticastLock? = null

  override fun onBind(intent: Intent?): IBinder? = null

  override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
    val manager = getSystemService(NotificationManager::class.java)
    if (Build.VERSION.SDK_INT >= 26) {
      manager.createNotificationChannel(
        NotificationChannel(CHANNEL, "Голосовая комната", NotificationManager.IMPORTANCE_LOW)
      )
    }
    val open = PendingIntent.getActivity(
      this,
      0,
      Intent(this, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP),
      PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT
    )
    val room = intent?.getStringExtra(EXTRA_ROOM)?.takeIf { it.isNotBlank() } ?: "Поигрулькины"
    val notification = NotificationCompat.Builder(this, CHANNEL)
      .setSmallIcon(R.drawable.ic_notification)
      .setColor(0xFF7C6CFF.toInt())
      .setContentTitle(room)
      .setContentText("Вы в голосовой комнате")
      .setContentIntent(open)
      .setOngoing(true)
      .setCategory(NotificationCompat.CATEGORY_CALL)
      .build()

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

  override fun onDestroy() {
    wakeLock?.takeIf { it.isHeld }?.release()
    wifiLock?.takeIf { it.isHeld }?.release()
    multicastLock?.takeIf { it.isHeld }?.release()
    wakeLock = null
    wifiLock = null
    multicastLock = null
    super.onDestroy()
  }
}
