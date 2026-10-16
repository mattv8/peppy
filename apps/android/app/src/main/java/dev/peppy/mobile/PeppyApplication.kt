package dev.peppy.mobile

import android.app.Application

/** Restores optional content-free relay handling before FCM delivers a restart wake. */
class PeppyApplication : Application() {
    override fun onCreate() {
        super.onCreate()
        RelayWakeRuntime.initialize(this)
        SmsCaptureWork.runner = SmsNativeCapture
        SmsCaptureLifecycle.ensureObserver(this)
    }
}
