package dev.peppy.mobile

import android.app.Application
import dev.peppy.mobile.diagnostics.DiagnosticsController
import dev.peppy.mobile.googlemessages.GoogleMessagesLink
import dev.peppy.mobile.sms.MmsCaptureLifecycle
import dev.peppy.mobile.sms.SmsCaptureLifecycle
import dev.peppy.mobile.sms.SmsNativeCapture


/** Restores optional content-free relay handling before FCM delivers a restart wake. */
class PeppyApplication : Application() {
    override fun onCreate() {
        super.onCreate()
        DiagnosticsController.initialize(this)
        RelayWakeRuntime.initialize(this)
        GoogleMessagesLink.onAppStart(this)
        SmsCaptureWork.runner = SmsNativeCapture
        SmsCaptureLifecycle.ensureObserver(this)
        MmsCaptureLifecycle.ensureObserver(this)
    }
}
