package dev.peppy.mobile.googlemessages

import android.os.SystemClock
import dev.peppy.mobile.gateway.GatewayTestBase
import dev.peppy.mobile.gateway.SharedVault
import dev.peppy.mobile.googlemessages.live.LiveTypingCredentials
import dev.peppy.mobile.googlemessages.live.LiveTypingEnableContext
import dev.peppy.mobile.googlemessages.live.LiveTypingListener
import java.util.UUID
import java.util.concurrent.Executor
import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import uniffi.peppy_mobile_bindings.NativeClient
import uniffi.peppy_mobile_bindings.NativeIncomingSms
import uniffi.peppy_mobile_bindings.NativeLiveClock
import uniffi.peppy_mobile_bindings.NativeLiveEnableContext
import uniffi.peppy_mobile_bindings.NativeLiveExecutor
import uniffi.peppy_mobile_bindings.NativeLiveOutbound
import uniffi.peppy_mobile_bindings.NativeLiveOutcome
import uniffi.peppy_mobile_bindings.NativeRcsLinkState
import uniffi.peppy_mobile_bindings.NativeRcsMappingVerification
import uniffi.peppy_mobile_bindings.NativeRcsReceiveContext
import uniffi.peppy_mobile_bindings.NativeTypingOrigin
import uniffi.peppy_mobile_bindings.NativeTypingState
import uniffi.peppy_mobile_bindings.gatewayPublishRcsEligibility
import uniffi.peppy_mobile_bindings.gatewayVerifyRcsMapping
import uniffi.peppy_mobile_bindings.rcsCanonicalizerRevision
import uniffi.peppy_mobile_bindings.NativeRcsEligibilityPublication

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class TypingBridgeRealCoreTest : GatewayTestBase() {
    @Test fun `real cores relay challenge and acknowledgement through the bridge`() {
        resetSharing()
        val gateway = enrollAndUnlock()
        val desktop = SharedVault.desktop
        val conversationId = UUID.randomUUID().toString()
        val googleConversationId = "google-${UUID.randomUUID()}"
        enableGatewayRouting(gateway, conversationId, System.currentTimeMillis() + DESKTOP_WALL_SKEW_MS)
        syncGatewayStateToDesktop(gateway, desktop)
        val host = RelayHost(desktop, false, deviceId) { clock(DESKTOP_WALL_SKEW_MS) }
        val effects = mutableListOf<LiveTypingEffectIntent>()
        val bridge = bridge(gateway, host, conversationId, googleConversationId, effects)
        enable(desktop, NativeLiveExecutor.NONE, DESKTOP_WALL_SKEW_MS)
        bridge.start(TypingCredentials("https://vault.example", "bearer", deviceId, 1))
        host.enable()

        assertTrue(relay(desktop, host, conversationId, DESKTOP_WALL_SKEW_MS))
        assertTrue(host.sentFrames > 0)
        assertTrue(effects.isEmpty())

        assertTrue(
            relay(
                desktop,
                host,
                conversationId,
                DESKTOP_WALL_SKEW_MS + REFRESH_INTERVAL_MS.toLong(),
                REFRESH_INTERVAL_MS,
            ),
        )
        val afterForwarding = SystemClock.elapsedRealtime().toULong()

        val effect = effects.single()
        assertEquals(googleConversationId, effect.conversationId)
        assertEquals(NativeTypingState.STARTED, effect.state)
        assertTrue(effect.deadlineMono > afterForwarding)
        assertTrue(effect.deadlineMono - afterForwarding <= TYPING_TTL_MS)
        assertTrue(effect.isCurrent.isCurrent())
    }

    @Test fun `discarded real-core replies cannot admit gateway typing`() {
        resetSharing()
        val gateway = enrollAndUnlock()
        val desktop = SharedVault.desktop
        val conversationId = UUID.randomUUID().toString()
        enableGatewayRouting(gateway, conversationId, System.currentTimeMillis() + DESKTOP_WALL_SKEW_MS)
        syncGatewayStateToDesktop(gateway, desktop)
        val host = RelayHost(desktop, true, deviceId) { clock(DESKTOP_WALL_SKEW_MS) }
        val effects = mutableListOf<LiveTypingEffectIntent>()
        val bridge = bridge(gateway, host, conversationId, "google-${UUID.randomUUID()}", effects)
        enable(desktop, NativeLiveExecutor.NONE, DESKTOP_WALL_SKEW_MS)
        bridge.start(TypingCredentials("https://vault.example", "bearer", deviceId, 1))
        host.enable()

        assertTrue(relay(desktop, host, conversationId, DESKTOP_WALL_SKEW_MS))
        assertTrue(
            relay(
                desktop,
                host,
                conversationId,
                DESKTOP_WALL_SKEW_MS + REFRESH_INTERVAL_MS.toLong(),
                REFRESH_INTERVAL_MS,
            ),
        )

        assertTrue(host.sentFrames > 0)
        assertTrue(effects.isEmpty())
    }

    private fun bridge(
        gateway: NativeClient,
        host: RelayHost,
        conversationId: String,
        googleConversationId: String,
        effects: MutableList<LiveTypingEffectIntent>,
    ): TypingBridge {
        lateinit var bridge: TypingBridge
        host.toGateway = { bridge.onFrame(it) }
        bridge = TypingBridge(
            context,
            host,
            Executor(Runnable::run),
            { RealTypingCore(gateway, conversationId, googleConversationId) },
            TypingSharingPreferences(context),
        ) { effects += it; "ok" }
        return bridge
    }

    private fun enable(client: NativeClient, executor: NativeLiveExecutor, clockOffset: Long = 0L) {
        val localDeviceId = if (executor == NativeLiveExecutor.NONE) SharedVault.desktopDeviceId else deviceId
        client.enableLiveTyping(
            NativeLiveEnableContext(listOf(deviceId, SharedVault.desktopDeviceId), localDeviceId, executor),
            clock(clockOffset),
        )
        client.setTypingSharing(true)
    }

    private fun relay(
        client: NativeClient,
        host: RelayHost,
        conversationId: String,
        wallOffset: Long = 0L,
        monoOffset: ULong = 0UL,
    ): Boolean {
        val frame = client.sealTypingSignalV2(
            conversationId,
            deviceId,
            NativeTypingOrigin.VAULT_DEVICE,
            null,
            NativeTypingState.STARTED,
            clock(wallOffset, monoOffset),
        ) ?: return false
        host.toGateway(frame.frameJson)
        return true
    }

    private fun enableGatewayRouting(gateway: NativeClient, conversationId: String, now: Long) {
        gateway.captureIncoming(NativeIncomingSms(conversationId, PEER, "", "typing-$conversationId", true))
        gateway.recordRcsLinkLifecycle(NativeRcsLinkState.LINKED, true, true, true, now, now)
        assertEquals(
            NativeRcsMappingVerification.MAPPED,
            gatewayVerifyRcsMapping(gateway, rcsObservation(now), now),
        )
        gatewayPublishRcsEligibility(gateway, listOf(NativeRcsEligibilityPublication(conversationId)), now)
    }

    private fun rcsObservation(now: Long): String {
        val outgoing = JSONObject()
            .put("outgoing_id", "google-outgoing")
            .put("sim_participant_id", "google-outgoing")
            .put("own_identity_raw", OWN_NUMBER)
            .put("sim_payload", JSONObject().put("two", 2).put("sim_number", 7))
            .put("rcs_chats_enabled", true)
        return JSONObject()
            .put("google_conversation_id", "google-typing")
            .put("google_candidate_count", 1)
            .put("participants", JSONArray().put(JSONObject()
                .put("identity", PEER)
                .put("google_participant_id", "google-peer")
                .put("identifier_type", 1)
                .put("is_visible", true)
                .put("is_me", false)))
            .put("conversation_type", "Rcs")
            .put("status", "eligible")
            .put("send_mode", "Automatic")
            .put("read_only", false)
            .put("default_outgoing", true)
            .put("selected_sim_id", "android-subscription-7")
            .put("selected_sim_own_identity", OWN_NUMBER)
            .put("carrier_binding", JSONObject()
                .put("carrier_route_id", "android-subscription-7")
                .put("android_subscription_id", 7)
                .put("is_current_default_sms", true)
                .put("own_number_raw", OWN_NUMBER)
                .put("own_number_source", "SubscriptionManagerPhoneNumber")
                .put("route_observed_at_ms", now)
                .put("route_context_revision", 1)
                .put("same_handset_evidence_id", "verified-handset"))
            .put("outgoing", outgoing)
            .put("native_evidence", JSONObject()
                .put("lifetime", JSONObject()
                    .put("runtime_id", "runtime")
                    .put("run_generation", 1)
                    .put("link_generation", 1)
                    .put("session_generation", 1)
                    .put("context_revision", 1))
                .put("lookup_id", "lookup")
                .put("lookup_complete", true)
                .put("google_candidate_count", 1)
                .put("settings_revision", 1)
                .put("settings_provenance", "CurrentEvent")
                .put("settings_observed_at_ms", now)
                .put("settings_stream_epoch", 1)
                .put("settings_currentness", "ContinuousVerifiedFeed")
                .put("current_settings_sims", JSONArray().put(outgoing))
                .put("settings_validated_at_ms", now)
                .put("conversation_observed_at_ms", now))
            .put("identity_region", "US")
            .put("observed_at_ms", now)
            .put("context_revision", 1)
            .put("link_generation", 1)
            .put("session_generation", 1)
            .put("canonicalizer_revision", rcsCanonicalizerRevision().toLong())
            .put("mapping_revision", 1)
            .put("link_state", "Linked")
            .toString()
    }

    private fun syncGatewayStateToDesktop(gateway: NativeClient, desktop: NativeClient) {
        var cursor = desktop.receiveCursor().toLong()
        gateway.pendingOutboxJson().forEach { wire ->
            gateway.ackOutbox(JSONObject(wire).getString("envelope_id"))
            desktop.ingestRaw(wire.toByteArray(), (++cursor).toString())
        }
        desktop.applyPending(50uL)
    }

    private fun resetSharing() {
        context.getSharedPreferences("google-messages-link", 0).edit().clear().commit()
    }

    private fun clock(wallOffset: Long = 0L, monoOffset: ULong = 0UL) = NativeLiveClock(
        System.currentTimeMillis() + wallOffset,
        SystemClock.elapsedRealtime().toULong() + monoOffset,
    )

    private class RelayHost(
        private val desktop: NativeClient,
        private val discard: Boolean,
        private val gatewayDeviceId: String,
        private val desktopClock: () -> NativeLiveClock,
    ) : TypingTransport {
        lateinit var listener: LiveTypingListener
        lateinit var toGateway: (String) -> Unit
        var sentFrames = 0
            private set
        override fun start(credentials: LiveTypingCredentials, listener: LiveTypingListener) { this.listener = listener }
        override fun stop() = Unit
        override fun send(frameJson: String): Boolean = false
        override fun send(frameJson: String, publication: Long): Boolean {
            if (publication != PUBLICATION) return false
            sentFrames++
            if (discard) return true
            desktop.openTypingSignalV2(frameJson, desktopClock())
                .replyFrameJson?.let {
                    toGateway(it)
                }
            return true
        }
        fun enable() = listener.onEnabled(
            LiveTypingEnableContext(
                listOf(SharedVault.desktopDeviceId, gatewayDeviceId),
                gatewayDeviceId,
                PUBLICATION,
            ),
        )
    }

    private companion object {
        const val PEER = "+12025550123"
        const val OWN_NUMBER = "+12025550101"
        const val PUBLICATION = 1L
        const val DESKTOP_WALL_SKEW_MS = -1_250L
        const val REFRESH_INTERVAL_MS = 3_001UL
        const val TYPING_TTL_MS = 15_000UL
    }

    private class RealTypingCore(
        private val client: NativeClient,
        private val conversationId: String,
        private val googleConversationId: String,
    ) : TypingCore {
        override fun enable(context: NativeLiveEnableContext, clock: NativeLiveClock) = client.enableLiveTyping(context, clock)
        override fun setSharing(enabled: Boolean) = client.setTypingSharing(enabled)
        override fun disable() = client.disableLiveTyping()
        override fun receiveContext(googleConversationId: String): TypingReceiveContext? = null
        override fun ownedGoogleConversationId(conversationId: String) = googleConversationId.takeIf { conversationId == this.conversationId }
        override fun seal(conversationId: String, gatewayDeviceId: String, participant: String?, state: NativeTypingState, clock: NativeLiveClock): NativeLiveOutbound? = null
        override fun open(frameJson: String, clock: NativeLiveClock): NativeLiveOutcome = client.openTypingSignalV2(frameJson, clock)
    }
}
