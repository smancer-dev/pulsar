package dev.pulsar.video

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.Service
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.IBinder

/**
 * M6: a foreground service that keeps the remote session (native decode + the
 * pulsar-core sockets) alive while the app is backgrounded. Started by the plugin
 * when a stream begins, stopped on detach/stop.
 */
class StreamService : Service() {
    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        val chan = "pulsar_stream"
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            getSystemService(NotificationManager::class.java).createNotificationChannel(
                NotificationChannel(chan, "Pulsar Stream", NotificationManager.IMPORTANCE_LOW)
            )
        }
        val builder = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            Notification.Builder(this, chan)
        } else {
            @Suppress("DEPRECATION") Notification.Builder(this)
        }
        val notif = builder
            .setContentTitle("Pulsar")
            .setContentText("Uzak oturum aktif")
            .setSmallIcon(android.R.drawable.ic_media_play)
            .setOngoing(true)
            .build()

        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            startForeground(1, notif, ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PLAYBACK)
        } else {
            startForeground(1, notif)
        }
        return START_STICKY
    }
}
