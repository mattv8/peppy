package dev.peppy.mobile

import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.assertTextEquals
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTextInput
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.test.SemanticsMatcher
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test

class OwnerPhoneApprovalScreenTest {
    @get:Rule val compose = createComposeRule()

    private class Actions(private val initial: OwnerPhoneApprovalHost.Result = verifyResult()) : OwnerPhoneApprovalActions {
        var approvals = 0
        val rotations = mutableListOf<OwnerPhoneMethod>()
        private val verify = verifyResult()
        override suspend fun start(method: OwnerPhoneMethod) = initial
        override suspend fun poll() = verify
        override suspend fun rotateIfNeeded(requested: OwnerPhoneMethod): OwnerPhoneApprovalHost.Result {
            rotations += requested
            return initial as? OwnerPhoneApprovalHost.Result.View ?: verify
        }
        override suspend fun restart(method: OwnerPhoneMethod) = initial
        override suspend fun approve(prompt: SecretPrompt): OwnerPhoneApprovalHost.Result {
            val secret = prompt.ask() ?: return OwnerPhoneApprovalHost.Result.Cancelled
            secret.fill('\u0000')
            approvals += 1
            return OwnerPhoneApprovalHost.Result.View(verify.status.copy(stage = OwnerPhoneStage.CONNECTED), null)
        }
        override suspend fun cancel() = OwnerPhoneApprovalHost.Result.Cancelled
    }

    private class RecordingSecureWindow : SecureWindow {
        val calls = mutableListOf<Boolean>()
        override fun setSecure(enabled: Boolean) {
            calls += enabled
        }
    }

    private companion object {
        fun verifyResult() = OwnerPhoneApprovalHost.Result.View(
            OwnerPhoneStatus(OwnerPhoneStage.VERIFY, 30, "123456", "claim", "invite"),
            null,
        )

        fun waitingResult() = OwnerPhoneApprovalHost.Result.View(
            OwnerPhoneStatus(OwnerPhoneStage.WAITING, 30, null, null, "invite"),
            "qr",
        )

        fun string(id: Int): String = InstrumentationRegistry.getInstrumentation().targetContext.getString(id)

        val noReveal: suspend () -> RevealOutcome = { RevealOutcome.Cancelled }
    }

    @Test fun codesMatchCheckboxGatesOneApproval() {
        val actions = Actions()
        compose.setContent {
            OwnerPhoneApprovalScreen(actions, {}, noReveal, RecordingSecureWindow())
        }

        compose.onNodeWithTag("owner-phone-approve").assertIsNotEnabled()
        compose.onNodeWithTag("owner-phone-codes-match").performClick()
        compose.onNodeWithTag("owner-phone-approve").performClick()
        compose.onNodeWithTag("owner-phone-prompt-field").performTextInput("vault-secret")
        compose.onNodeWithTag("owner-phone-prompt-confirm").performClick()
        compose.onNodeWithTag("owner-phone-connected").assertIsDisplayed()
        assertEquals(1, actions.approvals)
    }

    @Test fun missingSecretShowsApprovalPromptDialog() {
        val actions = Actions()
        compose.setContent { OwnerPhoneApprovalScreen(actions, {}, noReveal, RecordingSecureWindow()) }

        compose.onNodeWithTag("owner-phone-codes-match").performClick()
        compose.onNodeWithTag("owner-phone-approve").performClick()

        compose.onNodeWithTag("owner-phone-approval-dialog").assertIsDisplayed()
        compose.onNodeWithTag("owner-phone-prompt-field").assertIsDisplayed()
        assertEquals(0, actions.approvals)
    }

    @Test fun cancellingApprovalPromptLeavesVerifyStageWithoutApproval() {
        val actions = Actions()
        compose.setContent { OwnerPhoneApprovalScreen(actions, {}, noReveal, RecordingSecureWindow()) }

        compose.onNodeWithTag("owner-phone-codes-match").performClick()
        compose.onNodeWithTag("owner-phone-approve").performClick()
        compose.onNodeWithTag("owner-phone-prompt-cancel").performClick()

        compose.onNodeWithTag("owner-phone-approval-dialog").assertDoesNotExist()
        compose.onNodeWithTag("owner-phone-sas").assertIsDisplayed()
        compose.onNodeWithTag("owner-phone-approve").assertIsNotEnabled()
        assertEquals(0, actions.approvals)
    }

    @Test fun waitingStateShowsFullSizeQrAndCancel() {
        compose.setContent { OwnerPhoneApprovalScreen(Actions(waitingResult()), {}, noReveal, RecordingSecureWindow()) }
        compose.onNodeWithTag("owner-phone-qr").assertIsDisplayed()
        compose.onNodeWithTag("owner-phone-cancel").assertIsDisplayed()
    }

    @Test fun sasHasASpacedVerificationCodeAccessibilityLabel() {
        compose.setContent { OwnerPhoneApprovalScreen(Actions(), {}, noReveal, RecordingSecureWindow()) }

        compose.onNodeWithTag("owner-phone-sas").assert(
            SemanticsMatcher.expectValue(
                SemanticsProperties.ContentDescription,
                listOf("${string(R.string.peppy_production_pairing_sas_label)} 1 2 3 4 5 6"),
            ),
        )
    }

    @Test fun failedStartShowsFixedErrorSurface() {
        compose.setContent {
            OwnerPhoneApprovalScreen(Actions(OwnerPhoneApprovalHost.Result.Failed("offline")), {}, noReveal, RecordingSecureWindow())
        }
        compose.onNodeWithTag("owner-phone-error").assertIsDisplayed()
    }

    @Test fun revealMissingSecretShowsSecretUnavailable() {
        compose.setContent {
            OwnerPhoneApprovalScreen(
                Actions(waitingResult()),
                {},
                { RevealOutcome.Failed("vault-secret-unavailable") },
                RecordingSecureWindow(),
            )
        }
        compose.onNodeWithTag("owner-phone-shared-secret").performClick()

        compose.onNodeWithTag("owner-phone-reveal-error").assertTextEquals(string(R.string.peppy_owner_phone_error_secret_unavailable))
        compose.onNodeWithTag("owner-phone-revealed-secret").assertDoesNotExist()
    }

    @Test fun revealSwitchesTheInvitationToSharedSecretBeforeReadingTheSecret() {
        val actions = Actions(waitingResult())
        var revealCalls = 0
        compose.setContent {
            OwnerPhoneApprovalScreen(
                actions,
                {},
                {
                    assertEquals(listOf(OwnerPhoneMethod.SHARED_SECRET), actions.rotations)
                    revealCalls += 1
                    RevealOutcome.Revealed("shown-secret".toCharArray())
                },
                RecordingSecureWindow(),
            )
        }

        compose.onNodeWithTag("owner-phone-shared-secret").performClick()

        assertEquals(1, revealCalls)
    }

    @Test fun approvedInvitationShowsApprovedCopyWithoutARestartAction() {
        val approved = OwnerPhoneApprovalHost.Result.View(
            OwnerPhoneStatus(OwnerPhoneStage.APPROVED, 30, null, "claim", "invite"),
            null,
        )
        compose.setContent { OwnerPhoneApprovalScreen(Actions(approved), {}, noReveal, RecordingSecureWindow()) }

        compose.onNodeWithText(string(R.string.peppy_owner_phone_approved_body)).assertIsDisplayed()
        compose.onNodeWithTag("owner-phone-restart").assertDoesNotExist()
    }

    @Test fun deniedInvitationShowsTheDeniedMessageAndRestartAction() {
        val denied = OwnerPhoneApprovalHost.Result.View(
            OwnerPhoneStatus(OwnerPhoneStage.DENIED, 30, null, null, "invite"),
            null,
        )
        compose.setContent { OwnerPhoneApprovalScreen(Actions(denied), {}, noReveal, RecordingSecureWindow()) }

        compose.onNodeWithTag("owner-phone-error").assertTextEquals(string(R.string.peppy_owner_phone_error_denied))
        compose.onNodeWithTag("owner-phone-restart").assertIsDisplayed()
    }

    @Test fun revealReadFailureShowsSecureStorageError() {
        compose.setContent {
            OwnerPhoneApprovalScreen(
                Actions(waitingResult()),
                {},
                { RevealOutcome.Failed("secure-store-unavailable") },
                RecordingSecureWindow(),
            )
        }
        compose.onNodeWithTag("owner-phone-shared-secret").performClick()

        compose.onNodeWithTag("owner-phone-reveal-error").assertTextEquals(string(R.string.peppy_production_secure_storage_failed))
    }

    @Test fun revealWithoutSecureLockShowsLockRequiredError() {
        compose.setContent {
            OwnerPhoneApprovalScreen(
                Actions(waitingResult()),
                {},
                { RevealOutcome.Failed("lock-required") },
                RecordingSecureWindow(),
            )
        }
        compose.onNodeWithTag("owner-phone-shared-secret").performClick()

        compose.onNodeWithTag("owner-phone-reveal-error").assertTextEquals(string(R.string.peppy_owner_phone_error_lock_required))
    }

    @Test fun revealSetsFlagSecureUntilHidden() {
        val window = RecordingSecureWindow()
        compose.setContent {
            OwnerPhoneApprovalScreen(
                Actions(waitingResult()),
                {},
                { RevealOutcome.Revealed("shown-secret".toCharArray()) },
                window,
            )
        }
        compose.onNodeWithTag("owner-phone-shared-secret").performClick()

        compose.onNodeWithTag("owner-phone-revealed-secret").assertTextEquals("shown-secret")
        assertEquals(listOf(true), window.calls)

        compose.onNodeWithTag("owner-phone-shared-secret").performClick()

        compose.onNodeWithTag("owner-phone-revealed-secret").assertDoesNotExist()
        assertEquals(listOf(true, false), window.calls)
    }
}
