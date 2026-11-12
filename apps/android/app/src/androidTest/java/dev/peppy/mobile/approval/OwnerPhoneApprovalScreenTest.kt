package dev.peppy.mobile.approval

import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.test.SemanticsMatcher
import androidx.compose.ui.test.assert
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.assertTextEquals
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTextInput
import androidx.test.platform.app.InstrumentationRegistry
import dev.peppy.mobile.R
import dev.peppy.mobile.security.RevealOutcome
import dev.peppy.mobile.security.SecureWindow
import org.junit.Assert.*
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test



class OwnerPhoneApprovalScreenTest {
    @get:Rule val compose = createComposeRule()

    private class Actions(
        private val initial: OwnerPhoneApprovalHost.Result = verifyResult(),
        private val pollResults: ArrayDeque<OwnerPhoneApprovalHost.Result> = ArrayDeque(),
    ) : OwnerPhoneApprovalActions {
        var approvals = 0
        var approvalEntries = 0
        var polls = 0
        var restarts = 0
        val rotations = mutableListOf<OwnerPhoneMethod>()
        private val verify = verifyResult()
        override suspend fun start(method: OwnerPhoneMethod) = initial
        override suspend fun poll(): OwnerPhoneApprovalHost.Result {
            polls += 1
            return pollResults.removeFirstOrNull() ?: verify
        }
        override suspend fun rotateIfNeeded(requested: OwnerPhoneMethod): OwnerPhoneApprovalHost.Result {
            rotations += requested
            return initial as? OwnerPhoneApprovalHost.Result.View ?: verify
        }
        override suspend fun restart(method: OwnerPhoneMethod): OwnerPhoneApprovalHost.Result {
            restarts += 1
            return initial
        }
        override suspend fun approve(prompt: SecretPrompt): OwnerPhoneApprovalHost.Result {
            approvalEntries += 1
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
        fun verifyResult(
            invitationId: String = "invite",
            claimDigest: String? = "claim",
            sas: String? = "123456",
            expiresInSeconds: Int = 30,
        ) = OwnerPhoneApprovalHost.Result.View(
            OwnerPhoneStatus(OwnerPhoneStage.VERIFY, expiresInSeconds, sas, claimDigest, invitationId),
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
        assertEquals(1, actions.approvalEntries)
        assertEquals(1, actions.approvals)
    }

    @Test fun retryFromAnExpiredVerifyErrorPollsWithoutOpeningApproval() {
        val actions = Actions(pollResults = ArrayDeque(listOf(
            OwnerPhoneApprovalHost.Result.Failed("expired"),
            verifyResult(),
        )))
        compose.setContent { OwnerPhoneApprovalScreen(actions, {}, noReveal, RecordingSecureWindow()) }

        compose.mainClock.advanceTimeBy(2_000)
        compose.onNodeWithTag("owner-phone-error").assertIsDisplayed()
        compose.onNodeWithTag("owner-phone-codes-match").performClick()
        compose.onNodeWithText(string(R.string.peppy_try_again)).performClick()

        compose.onNodeWithTag("owner-phone-approval-dialog").assertDoesNotExist()
        assertEquals(2, actions.polls)
        assertEquals(0, actions.approvalEntries)
        assertEquals(0, actions.approvals)
        assertEquals(0, actions.restarts)
        assertTrue(actions.rotations.isEmpty())
    }

    @Test fun changedVerifyClaimClearsAcknowledgementButCountdownDoesNot() {
        val claimA = verifyResult(expiresInSeconds = 29)
        val claimB = verifyResult(invitationId = "invite-b", claimDigest = "claim-b", sas = "654321")
        val actions = Actions(pollResults = ArrayDeque(listOf(claimA, claimB)))
        compose.setContent { OwnerPhoneApprovalScreen(actions, {}, noReveal, RecordingSecureWindow()) }

        compose.onNodeWithTag("owner-phone-codes-match").performClick()
        compose.mainClock.advanceTimeBy(2_000)
        compose.onNodeWithTag("owner-phone-approve").assertIsEnabled()
        compose.mainClock.advanceTimeBy(2_000)

        compose.onNodeWithTag("owner-phone-approve").assertIsNotEnabled()
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
        compose.onNodeWithTag("owner-phone-approve").assertIsEnabled()
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
