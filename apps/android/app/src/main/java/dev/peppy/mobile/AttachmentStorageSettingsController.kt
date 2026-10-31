package dev.peppy.mobile

internal data class AttachmentStorageSnapshot(
    val usedBytes: Long,
    val limitBytes: Long,
    val baseBytes: Long,
    val cleanup: AttachmentStorageCleanup,
    val unavailableReason: AttachmentStorageUnavailableReason?,
    val notice: AttachmentStorageNotice,
    val noticeCopySupported: Boolean,
)

internal data class AttachmentStorageNotice(
    val policyVersion: String?,
    val state: AttachmentStorageNoticeState,
    val activationAtSeconds: Long?,
)

internal enum class AttachmentStorageCleanup { DISABLED, BLOCKED, PENDING, AVAILABLE }
internal enum class AttachmentStorageUnavailableReason { NOTICE_NOT_ELIGIBLE, DEVICE_NOT_READY, READ_ONLY, ENTITLEMENT_UNAVAILABLE, PROVIDER_RECONCILIATION_PENDING, BILLING_MUTATION_PENDING, CLEANUP_DISABLED }
internal enum class AttachmentStorageNoticeState { NOT_PRESENTED, GRACE_PERIOD, ELIGIBLE }

internal sealed interface AttachmentStorageLoad {
    data class Available(val status: AttachmentStorageSnapshot) : AttachmentStorageLoad
    data object Unsupported : AttachmentStorageLoad
    data object TemporarilyUnavailable : AttachmentStorageLoad
    data object AuthorizationFailure : AttachmentStorageLoad
    data object OrdinaryFailure : AttachmentStorageLoad
}

internal sealed interface AttachmentStorageUiState {
    data object Loading : AttachmentStorageUiState
    data object Unsupported : AttachmentStorageUiState
    data class Available(val status: AttachmentStorageSnapshot) : AttachmentStorageUiState
    data class Unavailable(val lastKnown: AttachmentStorageSnapshot?) : AttachmentStorageUiState
}

/** Session-scoped state only. Transport owns credentials and validates the binding around awaits. */
internal class AttachmentStorageSettingsController {
    var state: AttachmentStorageUiState = AttachmentStorageUiState.Loading
        private set
    private var scope: String? = null
    private var acknowledgement: AcknowledgementState = AcknowledgementState.NONE

    fun begin(scope: String) {
        if (scope == this.scope) return
        this.scope = scope
        acknowledgement = AcknowledgementState.NONE
        state = AttachmentStorageUiState.Loading
    }

    fun apply(scope: String, load: AttachmentStorageLoad): Boolean {
        if (scope != this.scope) return false
        state = when (load) {
            is AttachmentStorageLoad.Available -> AttachmentStorageUiState.Available(load.status)
            AttachmentStorageLoad.Unsupported -> AttachmentStorageUiState.Unsupported
            AttachmentStorageLoad.TemporarilyUnavailable -> AttachmentStorageUiState.Unavailable(state.lastKnown())
            AttachmentStorageLoad.AuthorizationFailure, AttachmentStorageLoad.OrdinaryFailure -> AttachmentStorageUiState.Unavailable(state.lastKnown())
        }
        return true
    }

    /** Call only after the complete disclosure is visible in the detail screen. */
    fun claimDisclosureRendered(scope: String): Boolean {
        val status = (state as? AttachmentStorageUiState.Available)?.status ?: return false
        if (scope != this.scope || !status.noticeCopySupported || status.notice.state != AttachmentStorageNoticeState.NOT_PRESENTED || acknowledgement != AcknowledgementState.NONE) return false
        acknowledgement = AcknowledgementState.IN_FLIGHT
        return true
    }

    fun acknowledgeResult(scope: String, succeeded: Boolean): Boolean {
        if (scope != this.scope || acknowledgement != AcknowledgementState.IN_FLIGHT) return false
        acknowledgement = if (succeeded) AcknowledgementState.SUCCEEDED else AcknowledgementState.FAILED
        return succeeded
    }

    fun retryFailedAcknowledgement(scope: String): Boolean {
        val status = (state as? AttachmentStorageUiState.Available)?.status
        if (scope != this.scope || acknowledgement != AcknowledgementState.FAILED || status?.let { !it.noticeCopySupported || it.notice.state != AttachmentStorageNoticeState.NOT_PRESENTED } != false) return false
        acknowledgement = AcknowledgementState.IN_FLIGHT
        return true
    }

    private enum class AcknowledgementState { NONE, IN_FLIGHT, FAILED, SUCCEEDED }

    private fun AttachmentStorageUiState.lastKnown() = when (this) {
        is AttachmentStorageUiState.Available -> status
        is AttachmentStorageUiState.Unavailable -> lastKnown
        AttachmentStorageUiState.Loading, AttachmentStorageUiState.Unsupported -> null
    }
}
