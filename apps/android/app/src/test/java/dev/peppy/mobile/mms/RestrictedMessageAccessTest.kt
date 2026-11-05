package dev.peppy.mobile.mms

import android.app.AppOpsManager
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class RestrictedMessageAccessTest {
    @Test fun statusMappingOnlyTreatsAllowedModeAsAllowed() {
        assertEquals(RestrictedMessageAccess.ALLOWED, restrictedMessageAccessForCheck { AppOpsManager.MODE_ALLOWED })
        assertEquals(RestrictedMessageAccess.NOT_ALLOWED, restrictedMessageAccessForCheck { AppOpsManager.MODE_ERRORED })
        assertEquals(RestrictedMessageAccess.UNSUPPORTED, restrictedMessageAccessForCheck { throw IllegalArgumentException() })
        assertEquals(RestrictedMessageAccess.UNKNOWN, restrictedMessageAccessForCheck { throw SecurityException() })
    }

    @Test fun commandUsesTheActualPackageName() {
        val command = restrictedMessageAppOpCommand("dev.example.peppy")

        assertTrue(command.contains("dev.example.peppy"))
        assertTrue(command.contains(READ_RESTRICTED_MESSAGES_OP))
    }

    @Test fun rescanPersistsBeforeEnqueueAndCoalescesExistingWork() {
        val calls = mutableListOf<String>()
        val outcome = requestProviderRescan(
            RestrictedMessageAccess.ALLOWED,
            observedMaxId = { calls += "high-water"; 42L },
            request = { observed -> calls += "request:$observed"; false },
            enqueue = { calls += "enqueue" },
        )

        assertEquals(ProviderRescanRequestOutcome.ALREADY_QUEUED, outcome)
        assertEquals(listOf("high-water", "request:42", "enqueue"), calls)
    }

    @Test fun unavailableAccessDoesNotReadOrScheduleARescan() {
        var called = false
        val outcome = requestProviderRescan(
            RestrictedMessageAccess.NOT_ALLOWED,
            observedMaxId = { called = true; 42L },
            request = { called = true; true },
            enqueue = { called = true },
        )

        assertEquals(ProviderRescanRequestOutcome.UNAVAILABLE, outcome)
        assertFalse(called)
    }

    @Test fun clickTimeGateRequiresEnabledMmsPermissionsOpenDatabaseAndCurrentAccess() {
        assertTrue(canRequestProviderRescan(true, true, true, RestrictedMessageAccess.ALLOWED))
        assertFalse(canRequestProviderRescan(false, true, true, RestrictedMessageAccess.ALLOWED))
        assertFalse(canRequestProviderRescan(true, false, true, RestrictedMessageAccess.ALLOWED))
        assertFalse(canRequestProviderRescan(true, true, false, RestrictedMessageAccess.ALLOWED))
        assertFalse(canRequestProviderRescan(true, true, true, RestrictedMessageAccess.NOT_ALLOWED))
    }

    @Test fun closedOrLockedCoreFailureDoesNotEnqueue() {
        var enqueued = false
        val outcome = requestProviderRescan(
            RestrictedMessageAccess.ALLOWED,
            observedMaxId = { 42L },
            request = { throw IllegalStateException("database closed") },
            enqueue = { enqueued = true },
        )

        assertEquals(ProviderRescanRequestOutcome.FAILED, outcome)
        assertFalse(enqueued)
    }

    @Test fun revokedAccessBetweenHighWaterAndRequestDoesNotEnqueue() {
        var access = RestrictedMessageAccess.ALLOWED
        var enqueued = false
        val outcome = requestProviderRescan(
            RestrictedMessageAccess.ALLOWED,
            observedMaxId = { access = RestrictedMessageAccess.NOT_ALLOWED; 42L },
            request = {
                if (access != RestrictedMessageAccess.ALLOWED) throw SecurityException("access revoked")
                true
            },
            enqueue = { enqueued = true },
        )

        assertEquals(ProviderRescanRequestOutcome.UNAVAILABLE, outcome)
        assertFalse(enqueued)
    }
}
