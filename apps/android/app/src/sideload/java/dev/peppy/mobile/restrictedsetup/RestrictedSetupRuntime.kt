package dev.peppy.mobile.restrictedsetup

import android.content.Context
import android.content.Intent
import android.os.Build
import androidx.core.content.ContextCompat
import dev.peppy.mobile.mms.RestrictedMessageAccess
import dev.peppy.mobile.mms.restrictedMessageAccess
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.launchIn
import kotlinx.coroutines.flow.onEach
import kotlinx.coroutines.withContext

internal object RestrictedSetupRuntime {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    @Volatile private var instance: RestrictedSetupController? = null
    @Volatile private var notifierInstance: RestrictedSetupNotifierImpl? = null

    fun isSupported(context: Context): Boolean =
        Build.VERSION.SDK_INT >= Build.VERSION_CODES.R && transportFactoryAvailable()

    fun controller(context: Context): RestrictedSetupController {
        val existing = instance
        if (existing != null) return existing
        return synchronized(this) {
            instance ?: createController(context.applicationContext).also { controller ->
                instance = controller
                controller.state.onEach { state ->
                    if (state.stage in TERMINAL_STAGES) context.applicationContext.stopService(serviceIntent(context))
                }.launchIn(scope)
            }
        }
    }

    fun startService(context: Context) {
        try {
            ContextCompat.startForegroundService(context, serviceIntent(context))
        } catch (_: SecurityException) {
            controller(context).reportForegroundUnavailable()
        } catch (_: IllegalStateException) {
            controller(context).reportForegroundUnavailable()
        }
    }

    fun notifier(context: Context): RestrictedSetupNotifierImpl {
        val existing = notifierInstance
        if (existing != null) return existing
        return synchronized(this) {
            notifierInstance ?: RestrictedSetupNotifierImpl(context.applicationContext).also { notifierInstance = it }
        }
    }

    internal fun createController(context: Context): RestrictedSetupController {
        deleteStaleCleanupPreferences(context)
        val addresses = AndroidLocalAddressSupplier(context)
        return RestrictedSetupController(
            controllerScope = scope,
            transportFactory = RestrictedSetupTransportFactory { AdbRestrictedSetupTransport(context.packageName) },
            accessReadback = RestrictedSetupAccessReadback {
                withContext(Dispatchers.IO) { restrictedMessageAccess(context) == RestrictedMessageAccess.ALLOWED }
            },
            localAddresses = addresses,
            discovery = NsdAdbServiceDiscovery(context, addresses),
            notifier = notifier(context),
            wirelessDebuggingReadback = WirelessDebuggingSettingReadback(context),
        )
    }

    private fun serviceIntent(context: Context) = Intent(context, RestrictedSetupService::class.java)

    private fun transportFactoryAvailable(): Boolean = runCatching {
        Class.forName("io.github.muntashirakon.adb.AbsAdbConnectionManager")
        Class.forName("org.conscrypt.Conscrypt")
    }.isSuccess

    internal fun deleteStaleCleanupPreferences(context: Context) {
        context.deleteSharedPreferences("restricted_setup")
    }

    private val TERMINAL_STAGES = setOf(
        RestrictedSetupStage.INTRODUCTION,
        RestrictedSetupStage.COMPLETE,
        RestrictedSetupStage.FAILED,
    )
}
