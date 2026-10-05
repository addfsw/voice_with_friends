package io.github.addfsw.poigrulkiny

import android.content.Context
import android.media.AudioDeviceCallback
import android.media.AudioDeviceInfo
import android.media.AudioManager
import android.os.Build
import android.os.Handler
import android.os.Looper
import androidx.annotation.RequiresApi

/// Звук на время звонка: режим «разговор» (в нём телефон включает
/// эхоподавление для микрофона, см. client/src/aaudio.rs) и куда идёт голос
/// собеседников.
///
/// Куда — выбирает человек в настройках:
/// - `auto` — в гарнитуру или наушники, если подключены, иначе в громкий
///   динамик (как громкая связь: телефон можно не держать у уха);
/// - `speaker` — всегда громкий динамик;
/// - `earpiece` — динамик у уха, как в обычном звонке.
class CallAudio(context: Context) {
  private val audio = context.getSystemService(AudioManager::class.java)
  private val handler = Handler(Looper.getMainLooper())
  private var active = false
  private var route = "auto"
  private var scoStarted = false

  /// Подключили или отключили наушники — выбираем заново.
  private val devices = object : AudioDeviceCallback() {
    override fun onAudioDevicesAdded(added: Array<out AudioDeviceInfo>) = reapply()
    override fun onAudioDevicesRemoved(removed: Array<out AudioDeviceInfo>) = reapply()
  }

  fun start() {
    if (active) return
    active = true
    audio.mode = AudioManager.MODE_IN_COMMUNICATION
    audio.registerAudioDeviceCallback(devices, handler)
    apply()
  }

  fun stop() {
    if (!active) return
    active = false
    audio.unregisterAudioDeviceCallback(devices)
    if (Build.VERSION.SDK_INT >= 31) {
      audio.clearCommunicationDevice()
    } else {
      @Suppress("DEPRECATION")
      audio.isSpeakerphoneOn = false
      stopSco()
    }
    audio.mode = AudioManager.MODE_NORMAL
  }

  fun setRoute(value: String) {
    route = value
    if (active) apply()
  }

  private fun reapply() {
    if (active) apply()
  }

  private fun apply() {
    if (Build.VERSION.SDK_INT >= 31) applyModern() else applyLegacy()
  }

  @RequiresApi(31)
  private fun applyModern() {
    val available = audio.availableCommunicationDevices
    // Первое доступное устройство из `types` (в порядке предпочтения).
    fun find(vararg types: Int): AudioDeviceInfo? {
      for (type in types) {
        available.firstOrNull { it.type == type }?.let { return it }
      }
      return null
    }
    val speaker = find(AudioDeviceInfo.TYPE_BUILTIN_SPEAKER)
    val target = when (route) {
      "speaker" -> speaker
      "earpiece" -> find(AudioDeviceInfo.TYPE_BUILTIN_EARPIECE)
      else -> find(*HEADSETS) ?: speaker
    }
    if (target != null) {
      audio.setCommunicationDevice(target)
    } else {
      audio.clearCommunicationDevice()
    }
  }

  @Suppress("DEPRECATION")
  private fun applyLegacy() {
    val outputs = audio.getDevices(AudioManager.GET_DEVICES_OUTPUTS)
    val wired = outputs.any { it.type in WIRED }
    val bluetooth = outputs.any { it.type == AudioDeviceInfo.TYPE_BLUETOOTH_SCO }
    when {
      route == "speaker" -> {
        stopSco()
        audio.isSpeakerphoneOn = true
      }
      route == "earpiece" -> {
        stopSco()
        audio.isSpeakerphoneOn = false
      }
      // Проводные наушники система выбирает сама, когда громкая связь выключена.
      wired -> {
        stopSco()
        audio.isSpeakerphoneOn = false
      }
      bluetooth -> {
        audio.isSpeakerphoneOn = false
        if (!scoStarted) {
          scoStarted = true
          audio.startBluetoothSco()
          audio.isBluetoothScoOn = true
        }
      }
      else -> {
        stopSco()
        audio.isSpeakerphoneOn = true
      }
    }
  }

  @Suppress("DEPRECATION")
  private fun stopSco() {
    if (!scoStarted) return
    scoStarted = false
    audio.isBluetoothScoOn = false
    audio.stopBluetoothSco()
  }

  private companion object {
    val WIRED = intArrayOf(
      AudioDeviceInfo.TYPE_WIRED_HEADSET,
      AudioDeviceInfo.TYPE_WIRED_HEADPHONES,
      AudioDeviceInfo.TYPE_USB_HEADSET,
    ).toList()

    /// В порядке предпочтения: беспроводная гарнитура, затем проводная.
    val HEADSETS = intArrayOf(
      AudioDeviceInfo.TYPE_BLE_HEADSET,
      AudioDeviceInfo.TYPE_BLUETOOTH_SCO,
      AudioDeviceInfo.TYPE_WIRED_HEADSET,
      AudioDeviceInfo.TYPE_WIRED_HEADPHONES,
      AudioDeviceInfo.TYPE_USB_HEADSET,
    )
  }
}
