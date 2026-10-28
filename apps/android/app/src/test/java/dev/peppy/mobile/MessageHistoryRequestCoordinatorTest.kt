package dev.peppy.mobile

import java.lang.reflect.Proxy
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import uniffi.peppy_mobile_bindings.NativeClientInterface
import uniffi.peppy_mobile_bindings.NativeMessageHistorySourceFacts

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class MessageHistoryRequestCoordinatorTest {
    @Test fun publishesEveryPassAndForcesOnlyTheFirstSuccessfulCallForAClient() {
        val forces = mutableListOf<Boolean>()
        var attempts = 0
        val client = client { method, args ->
            when (method) {
                "publishMessageHistoryCaptureSource" -> {
                    val facts = args.single() as NativeMessageHistorySourceFacts
                    forces += facts.forceRepublish
                    attempts++
                    if (attempts == 1) error("publication unavailable")
                    "{}"
                }
                else -> error("unexpected $method")
            }
        }
        val coordinator = MessageHistoryRequestCoordinator(client)

        assertTrue(runCatching { coordinator.publish("provider", true, false, true, true) }.isFailure)
        coordinator.publish("provider", true, false, true, true)
        coordinator.publish("provider", true, false, true, true)

        assertEquals(listOf(true, true, false), forces)
    }

    @Test fun finishingAZeroCaptureClaimWakesUploadWhenCorePublishesControl() {
        var uploadWakes = 0
        val client = client { method, _ ->
            when (method) {
                "finishMessageHistoryCapture" -> "{}"
                "drainMessageHistoryCaptureResults" -> """{"published":1,"pending":0}"""
                else -> error("unexpected $method")
            }
        }

        MessageHistoryRequestCoordinator(client) { uploadWakes++ }.finish(
            "provider",
            MessageHistoryRequestCoordinator.Admission("claim", 100),
            observations = 0,
            exhausted = false,
            reason = "capacity",
        )

        assertEquals(1, uploadWakes)
    }

    @Test fun invalidCoreBudgetsAndObservationCountsAreRejectedInsteadOfClamped() {
        var finishCalls = 0
        val client = client { method, _ ->
            when (method) {
                "beginMessageHistoryCapture" -> """{"token":"claim","budget":101}"""
                "finishMessageHistoryCapture" -> {
                    finishCalls++
                    "{}"
                }
                else -> error("unexpected $method")
            }
        }
        val coordinator = MessageHistoryRequestCoordinator(client)

        assertTrue(runCatching { coordinator.begin("provider", "sms") }.isFailure)
        assertTrue(
            runCatching {
                coordinator.finish("provider", MessageHistoryRequestCoordinator.Admission("claim", 100), 101, exhausted = false)
            }.isFailure,
        )
        assertEquals(0, finishCalls)
    }

    @Test fun readinessFlagsScheduleOnlyTheUnclaimedTransportAfterDrain() {
        val calls = mutableListOf<String>()
        val client = client { method, _ ->
            calls += method
            when (method) {
                "publishMessageHistoryCaptureSource" -> "{}"
                "drainMessageHistoryCaptureResults" -> """{"published":0,"pending":0}"""
                "pendingMessageHistoryCaptureRequests" -> """{"requests":[{"requestId":"claimed"}],"needsSms":true,"needsMms":false}"""
                else -> error("unexpected $method")
            }
        }

        GatewayWork.publishAndScheduleMessageHistory(
            client, "provider", smsAvailable = true, mmsAvailable = true,
            permission = true, policy = true,
            smsCaptureAllowed = true, mmsCaptureAllowed = true,
            enqueueSms = { calls += "sms" }, enqueueMms = { calls += "mms" },
        )

        assertEquals(
            listOf(
                "publishMessageHistoryCaptureSource",
                "drainMessageHistoryCaptureResults",
                "pendingMessageHistoryCaptureRequests",
                "sms",
            ),
            calls,
        )
    }

    @Test fun claimedRequestsAndMalformedOptionalJsonDoNotCreateCaptureLoops() {
        var pending = """{"requests":[{"requestId":"claimed"}],"needsSms":false,"needsMms":false}"""
        val client = client { method, _ ->
            when (method) {
                "publishMessageHistoryCaptureSource" -> "{}"
                "drainMessageHistoryCaptureResults" -> """{"published":0,"pending":0}"""
                "pendingMessageHistoryCaptureRequests" -> pending
                else -> error("unexpected $method")
            }
        }
        var hints = 0
        fun pass() = GatewayWork.publishAndScheduleMessageHistory(
            client, "provider", true, true, true, true, true, true,
            enqueueSms = { hints++ }, enqueueMms = { hints++ },
        )

        pass()
        pending = "not-json"
        pass()

        assertEquals(0, hints)
    }

    @Test fun smsPolicyBlockedWhileMmsAllowedSettlesPolicyDeniedWhenSmsPermissionExists() {
        assertUnavailableReason(
            needsSms = true,
            needsMms = false,
            smsPermissionGranted = true,
            mmsPermissionGranted = true,
            smsCaptureAllowed = false,
            mmsCaptureAllowed = true,
            expectedTransport = "sms",
            expectedReason = "policy_denied",
        )
    }

    @Test fun mmsPolicyBlockedWhileSmsAllowedSettlesPolicyDeniedWhenMmsPermissionExists() {
        assertUnavailableReason(
            needsSms = false,
            needsMms = true,
            smsPermissionGranted = true,
            mmsPermissionGranted = true,
            smsCaptureAllowed = true,
            mmsCaptureAllowed = false,
            expectedTransport = "mms",
            expectedReason = "policy_denied",
        )
    }

    @Test fun missingMmsPermissionSettlesPermissionDeniedWhenMmsPolicyAllowsCapture() {
        assertUnavailableReason(
            needsSms = false,
            needsMms = true,
            smsPermissionGranted = true,
            mmsPermissionGranted = false,
            smsCaptureAllowed = true,
            mmsCaptureAllowed = false,
            expectedTransport = "mms",
            expectedReason = "permission_denied",
        )
    }

    private fun assertUnavailableReason(
        needsSms: Boolean,
        needsMms: Boolean,
        smsPermissionGranted: Boolean,
        mmsPermissionGranted: Boolean,
        smsCaptureAllowed: Boolean,
        mmsCaptureAllowed: Boolean,
        expectedTransport: String,
        expectedReason: String,
    ) {
        val beginTransports = mutableListOf<String>()
        val finishInputs = mutableListOf<JSONObject>()
        val client = client { method, args ->
            when (method) {
                "publishMessageHistoryCaptureSource" -> "{}"
                "drainMessageHistoryCaptureResults" -> """{"published":0,"pending":0}"""
                "pendingMessageHistoryCaptureRequests" ->
                    """{"requests":[],"needsSms":$needsSms,"needsMms":$needsMms}"""
                "beginMessageHistoryCapture" -> {
                    beginTransports += args[1] as String
                    """{"token":"claim","budget":100}"""
                }
                "finishMessageHistoryCapture" -> {
                    finishInputs += JSONObject(args.single() as String)
                    "{}"
                }
                else -> error("unexpected $method")
            }
        }

        GatewayWork.publishAndScheduleMessageHistory(
            client, "provider", smsAvailable = true, mmsAvailable = true,
            permission = true, policy = true,
            smsPermissionGranted = smsPermissionGranted,
            mmsPermissionGranted = mmsPermissionGranted,
            smsCaptureAllowed = smsCaptureAllowed,
            mmsCaptureAllowed = mmsCaptureAllowed,
            enqueueSms = { error("unavailable SMS must be settled") },
            enqueueMms = { error("unavailable MMS must be settled") },
        )

        assertEquals(listOf(expectedTransport), beginTransports)
        assertEquals(1, finishInputs.size)
        assertEquals(0, finishInputs.single().getInt("observations"))
        assertEquals(expectedReason, finishInputs.single().getString("reason"))
        assertFalse(finishInputs.single().getBoolean("exhausted"))
    }

    @Test fun notificationOnlyPolicyPublishesDisabledSourceWithoutSchedulingCapture() {
        lateinit var published: NativeMessageHistorySourceFacts
        val client = client { method, args ->
            when (method) {
                "publishMessageHistoryCaptureSource" -> {
                    published = args.single() as NativeMessageHistorySourceFacts
                    "{}"
                }
                "drainMessageHistoryCaptureResults" -> """{"published":0,"pending":0}"""
                "pendingMessageHistoryCaptureRequests" -> """{"requests":[],"needsSms":false,"needsMms":false}"""
                else -> error("unexpected $method")
            }
        }

        val uploadNeeded = GatewayWork.publishAndScheduleMessageHistory(
            client, "provider", smsAvailable = true, mmsAvailable = false,
            permission = false, policy = false,
            smsCaptureAllowed = false, mmsCaptureAllowed = false,
            enqueueSms = { error("disabled source") }, enqueueMms = { error("disabled source") },
        )

        assertTrue(published.sms)
        assertFalse(published.mms)
        assertFalse(published.permission)
        assertFalse(published.policy)
        assertFalse(uploadNeeded)
    }

    @Test fun publicationFailureIsNonFatalAndControlDrainStillRequestsUpload() {
        val client = client { method, _ ->
            when (method) {
                "publishMessageHistoryCaptureSource" -> error("optional publication failed")
                "drainMessageHistoryCaptureResults" -> """{"published":1,"pending":0}"""
                "pendingMessageHistoryCaptureRequests" -> """{"requests":[],"needsSms":false,"needsMms":false}"""
                else -> error("unexpected $method")
            }
        }

        val uploadNeeded = GatewayWork.publishAndScheduleMessageHistory(
            client, "provider", true, false, true, true, true, false,
            enqueueSms = { error("unused") }, enqueueMms = { error("unused") },
        )

        assertTrue(uploadNeeded)
    }

    private fun client(call: (String, List<Any>) -> Any?): NativeClientInterface =
        Proxy.newProxyInstance(javaClass.classLoader, arrayOf(NativeClientInterface::class.java)) { _, method, arguments ->
            call(method.name, arguments?.toList().orEmpty())
        } as NativeClientInterface
}
