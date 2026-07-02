package dev.pulsar.video

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.Service
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.IBinder

/** M16: foreground service of type mediaProjection — required to capture the screen. */
class HostService : Service() {
    companion object {
        /** True once startForeground(mediaProjection) has run — getMediaProjection is only
         *  legal AFTER this (Android 14+), so the plugin waits on it to avoid a race. */
        @Volatile
        var ready = false
    }

    override fun onBind(intent: Intent?): IBinder? = null
    override fun onDestroy() { ready = false; super.onDestroy() }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        val chan = "pulsar_host"
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            getSystemService(NotificationManager::class.java).createNotificationChannel(
                NotificationChannel(chan, "Pulsar Host", NotificationManager.IMPORTANCE_LOW)
            )
        }
        val b = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            Notification.Builder(this, chan)
        } else {
            @Suppress("DEPRECATION") Notification.Builder(this)
        }
        val n = b.setContentTitle("Pulsar")
            .setContentText("Ekran paylaşılıyor")
            .setSmallIcon(android.R.drawable.ic_menu_share)
            .setOngoing(true)
            .build()
        try {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                startForeground(2, n, ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PROJECTION)
            } else {
                startForeground(2, n)
            }
            ready = true
            android.util.Log.i("PulsarHostSvc", "startForeground OK (mediaProjection)")
        } catch (e: Exception) {
            android.util.Log.e("PulsarHostSvc", "startForeground FAILED", e)
        }
        return START_STICKY
    }
}
