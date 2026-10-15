package dev.peppy.mobile

import android.content.Context
import android.os.Build
import androidx.work.BackoffPolicy
import androidx.work.Constraints
import androidx.work.ExistingWorkPolicy
import androidx.work.NetworkType
import androidx.work.OneTimeWorkRequestBuilder
import androidx.work.OutOfQuotaPolicy
import androidx.work.WorkManager
import java.util.concurrent.TimeUnit

/** The sibling persistent gateway may install this handler; wake delivery never guesses a service. */
object GatewayWakeDispatch {
    @Volatile var reconnectHandler: (() -> Boolean)? = null

    fun dispatch(context: Context) {
        val handled = try {
            reconnectHandler?.invoke() == true
        } catch (_: Exception) {
            false
        }
        if (handled) return
        WorkManager.getInstance(context).enqueueUniqueWork(
            GatewayWork.NAME,
            ExistingWorkPolicy.APPEND_OR_REPLACE,
            createRequest(Build.VERSION.SDK_INT),
        )
    }

    internal fun createRequest(apiLevel: Int) = OneTimeWorkRequestBuilder<GatewaySyncWorker>()
        .setConstraints(Constraints.Builder().setRequiredNetworkType(NetworkType.CONNECTED).build())
        .setBackoffCriteria(BackoffPolicy.EXPONENTIAL, 30, TimeUnit.SECONDS)
        .apply {
            if (usesExpeditedRequest(apiLevel)) {
                setExpedited(OutOfQuotaPolicy.RUN_AS_NON_EXPEDITED_WORK_REQUEST)
            }
        }
        .build()

    /** API 26–30 uses ordinary work because [GatewaySyncWorker] has no foreground-info contract. */
    internal fun usesExpeditedRequest(apiLevel: Int) = apiLevel >= Build.VERSION_CODES.S
}
