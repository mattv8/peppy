package dev.peppy.mobile.restrictedsetup

import android.app.Service
import android.content.Intent
import android.os.Build
import android.os.IBinder
import androidx.core.app.ServiceCompat
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.launchIn
import kotlinx.coroutines.flow.onEach

internal class RestrictedSetupService : Service() {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        val controller = RestrictedSetupRuntime.controller(this)
        try {
            ServiceCompat.startForeground(this, RestrictedSetupNotifierImpl.NOTIFICATION_ID, RestrictedSetupRuntime.notifier(this).ongoing(), foregroundServiceType())
        } catch (_: SecurityException) {
            controller.reportForegroundUnavailable()
            stop()
            return START_NOT_STICKY
        } catch (_: IllegalStateException) {
            controller.reportForegroundUnavailable()
            stop()
            return START_NOT_STICKY
        }
        if (controller.state.value.stage in IDLE_STAGES) stop()
        else controller.state.onEach { state -> if (state.stage in IDLE_STAGES) stop() }.launchIn(scope)
        return START_NOT_STICKY
    }

    override fun onDestroy() {
        scope.coroutineContext.cancel()
        super.onDestroy()
    }

    override fun onBind(intent: Intent?): IBinder? = null

    private fun stop() {
        ServiceCompat.stopForeground(this, ServiceCompat.STOP_FOREGROUND_REMOVE)
        stopSelf()
    }

    private fun foregroundServiceType(): Int =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) android.content.pm.ServiceInfo.FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE else 0

    private companion object {
        val IDLE_STAGES = setOf(
            RestrictedSetupStage.INTRODUCTION,
            RestrictedSetupStage.COMPLETE,
            RestrictedSetupStage.FAILED,
        )
    }
}
