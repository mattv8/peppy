package dev.peppy.mobile

import org.junit.Assert.assertEquals
import org.junit.Test

class HostedAuthDiagnosticsTest {
    @Test fun enabledDiagnosticsEmitOnlyFixedEnumFields() {
        val records = mutableListOf<String>()

        HostedAuthDiagnostics(enabled = true, output = records::add).record(
            HostedAuthStage.CREDENTIAL_SDK,
            HostedAuthOutcome.CANCELLED,
        )

        assertEquals(listOf("stage=credential_sdk outcome=cancelled"), records)
    }

    @Test fun disabledDiagnosticsDoNotInvokeOutput() {
        HostedAuthDiagnostics(enabled = false, output = { error("diagnostics output invoked") }).record(
            HostedAuthStage.SESSION_EXCHANGE,
            HostedAuthOutcome.FAILURE,
        )
    }

    @Test fun diagnosticOutputFailureDoesNotEscapeTheAuthFlow() {
        HostedAuthDiagnostics(enabled = true, output = { error("log sink failed") }).record(
            HostedAuthStage.RECONCILIATION,
            HostedAuthOutcome.SESSION_EXPIRED,
        )
    }
}
