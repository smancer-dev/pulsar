package dev.pulsar.video

import android.accessibilityservice.AccessibilityService
import android.accessibilityservice.GestureDescription
import android.graphics.Path
import android.util.Log
import android.view.accessibility.AccessibilityEvent

/**
 * M16 polish: lets a remote peer CONTROL this phone. A normal app can't inject raw
 * touch events, but an AccessibilityService can `dispatchGesture`, so the mobile
 * host turns the peer's pointer (tap/swipe) into injected gestures. The user enables
 * it once in Settings → Accessibility.
 */
class PulsarA11yService : AccessibilityService() {
    companion object {
        @Volatile
        var instance: PulsarA11yService? = null
    }

    override fun onServiceConnected() { instance = this }
    override fun onDestroy() { instance = null; super.onDestroy() }
    override fun onAccessibilityEvent(event: AccessibilityEvent?) {}
    override fun onInterrupt() {}

    /** Tap (start≈end) or swipe between two screen-pixel points. */
    fun gesture(x1: Float, y1: Float, x2: Float, y2: Float) {
        val dist = Math.hypot((x2 - x1).toDouble(), (y2 - y1).toDouble())
        val ex = if (dist < 2) x1 + 1f else x2
        val ey = if (dist < 2) y1 + 1f else y2
        val path = Path().apply { moveTo(x1, y1); lineTo(ex, ey) }
        val dur = if (dist < 16) 60L else 250L
        Log.i("PulsarA11y", "gesture ($x1,$y1)->($ex,$ey) dur=$dur")
        try {
            val gd = GestureDescription.Builder()
                .addStroke(GestureDescription.StrokeDescription(path, 0, dur))
                .build()
            dispatchGesture(gd, null, null)
        } catch (_: Exception) {}
    }
}
