package dev.peppy.mobile

import android.app.Application
import dev.peppy.mobile.diagnostics.DiagnosticsController
import dev.peppy.mobile.gateway.SetupFinishTracker
import dev.peppy.mobile.googlemessages.GoogleMessagesLink
import dev.peppy.mobile.sms.MmsCaptureLifecycle
import dev.peppy.mobile.sms.SmsCaptureLifecycle
import dev.peppy.mobile.sms.SmsNativeCapture


/** Restores optional content-free relay handling before FCM delivers a restart wake. */
class PeppyApplication : Application() {
    /** The SMS "Setup finished" notice state for this process; shared by the SMS screen only. */
    internal val setupFinishTracker = SetupFinishTracker()

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
