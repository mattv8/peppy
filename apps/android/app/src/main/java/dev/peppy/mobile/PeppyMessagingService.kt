package dev.peppy.mobile

import com.google.firebase.messaging.FirebaseMessagingService
import com.google.firebase.messaging.RemoteMessage

/** FCM receives only `kind`/`value`; no envelope plaintext is accepted or displayed. */
class PeppyMessagingService : FirebaseMessagingService() {
    override fun onNewToken(token: String) {
        RelayWakeRuntime.tokenChanged(applicationContext)
    }

    override fun onMessageReceived(message: RemoteMessage) {
        if (!RelayWakeRuntime.acceptsWakes(applicationContext)) return
        when (message.data["kind"]) {
            "wake" -> GatewayWakeDispatch.dispatch(applicationContext)
            "challenge" -> {
                val value = message.data["value"] ?: return
                RelayWakeRuntime.challenge(applicationContext, value)
            }
        }
    }
}
