package io.github.addfsw.poigrulkiny

import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.os.Build
import androidx.core.app.NotificationCompat
import androidx.core.app.Person

/// Уведомление о сообщениях чата, пока программа свёрнута: одно на все
/// непрочитанные, как в мессенджерах. Телефон звенит на первом сообщении, а
/// следующие тихо добавляются в то же уведомление, пока его не откроют, —
/// чтобы не мешать игре. Вызывать из главного потока.
class ChatNotifier(private val context: Context) {
  companion object {
    private const val CHANNEL = "chat"
    private const val NOTIFICATION_ID = 2
    /// Сколько последних сообщений видно в уведомлении.
    private const val KEEP = 8
  }

  private class Message(val name: String, val text: String, val timeMs: Long)

  private val unread = ArrayDeque<Message>()
  private val manager = context.getSystemService(NotificationManager::class.java)

  fun show(room: String, name: String, text: String, timeMs: Long) {
    if (!manager.areNotificationsEnabled()) return
    if (Build.VERSION.SDK_INT >= 26) {
      manager.createNotificationChannel(
        NotificationChannel(CHANNEL, "Сообщения чата", NotificationManager.IMPORTANCE_DEFAULT)
      )
    }
    unread.addLast(Message(name, text, timeMs))
    while (unread.size > KEEP) unread.removeFirst()

    val style = NotificationCompat.MessagingStyle(Person.Builder().setName("Вы").build())
      .setConversationTitle(room.ifBlank { "Чат комнаты" })
      .setGroupConversation(true)
    for (m in unread) {
      style.addMessage(m.text, m.timeMs, Person.Builder().setName(m.name).build())
    }
    val open = PendingIntent.getActivity(
      context,
      NOTIFICATION_ID,
      Intent(context, MainActivity::class.java)
        .addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP)
        .putExtra(MainActivity.EXTRA_OPEN_CHAT, true),
      PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT
    )
    val notification = NotificationCompat.Builder(context, CHANNEL)
      .setSmallIcon(R.drawable.ic_notification)
      .setColor(0xFF7C6CFF.toInt())
      .setStyle(style)
      .setContentIntent(open)
      .setAutoCancel(true)
      .setOnlyAlertOnce(true)
      .setCategory(NotificationCompat.CATEGORY_MESSAGE)
      .build()
    manager.notify(NOTIFICATION_ID, notification)
  }

  /// Программу открыли или вышли из комнаты: сообщения прочитаны.
  fun clear() {
    unread.clear()
    manager.cancel(NOTIFICATION_ID)
  }
}
