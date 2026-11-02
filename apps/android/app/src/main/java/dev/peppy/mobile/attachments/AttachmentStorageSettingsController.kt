package dev.peppy.mobile.attachments


internal data class AttachmentStorageSnapshot(
    val usedBytes: Long,
    val limitBytes: Long,
    val baseBytes: Long,
    val cleanup: AttachmentStorageCleanup,
    val unavailableReason: AttachmentStorageUnavailableReason?,
)

internal enum class AttachmentStorageCleanup { DISABLED, BLOCKED, PENDING, AVAILABLE }
internal enum class AttachmentStorageUnavailableReason { UNKNOWN, DEVICE_NOT_READY, READ_ONLY, ENTITLEMENT_UNAVAILABLE, PROVIDER_RECONCILIATION_PENDING, BILLING_MUTATION_PENDING, CLEANUP_DISABLED }

internal sealed interface AttachmentStorageCapacityNotice {
    data object None : AttachmentStorageCapacityNotice
    data object NearFull : AttachmentStorageCapacityNotice
    data object FullCleanup : AttachmentStorageCapacityNotice
    data class NearFullBlocked(val causeKey: String) : AttachmentStorageCapacityNotice
    data class Cause(val key: String) : AttachmentStorageCapacityNotice
}

internal fun attachmentStorageCapacityNotice(status: AttachmentStorageSnapshot): AttachmentStorageCapacityNotice {
    if (status.cleanup == AttachmentStorageCleanup.PENDING) {
        return AttachmentStorageCapacityNotice.Cause("peppy.attachment_storage_cleanup_pending")
    }
    if (status.limitBytes <= 0 || status.usedBytes < 0) {
        return if (status.cleanup == AttachmentStorageCleanup.AVAILABLE) AttachmentStorageCapacityNotice.None
        else AttachmentStorageCapacityNotice.Cause(status.cleanupCauseKey())
    }
    val full = status.usedBytes >= status.limitBytes
    val nearFull = !full && status.usedBytes >= status.limitBytes - (status.limitBytes / 10)
    val cause = status.cleanupCauseKey()
    return when (status.cleanup) {
        AttachmentStorageCleanup.AVAILABLE -> when {
            full -> AttachmentStorageCapacityNotice.FullCleanup
            nearFull -> AttachmentStorageCapacityNotice.NearFull
            else -> AttachmentStorageCapacityNotice.None
        }
        AttachmentStorageCleanup.PENDING -> AttachmentStorageCapacityNotice.Cause("peppy.attachment_storage_cleanup_pending")
        AttachmentStorageCleanup.DISABLED, AttachmentStorageCleanup.BLOCKED -> when {
            full -> AttachmentStorageCapacityNotice.Cause(cause)
            nearFull -> AttachmentStorageCapacityNotice.NearFullBlocked(cause)
            else -> AttachmentStorageCapacityNotice.Cause(cause)
        }
    }
}

private fun AttachmentStorageSnapshot.cleanupCauseKey() = when (unavailableReason) {
    AttachmentStorageUnavailableReason.DEVICE_NOT_READY -> "peppy.attachment_storage_device_update_required"
    AttachmentStorageUnavailableReason.READ_ONLY -> "peppy.attachment_storage_read_only"
    AttachmentStorageUnavailableReason.BILLING_MUTATION_PENDING -> "peppy.attachment_storage_billing_pending"
    AttachmentStorageUnavailableReason.ENTITLEMENT_UNAVAILABLE,
    AttachmentStorageUnavailableReason.PROVIDER_RECONCILIATION_PENDING -> "peppy.attachment_storage_billing_unavailable"
    AttachmentStorageUnavailableReason.CLEANUP_DISABLED -> "peppy.attachment_storage_disabled"
    AttachmentStorageUnavailableReason.UNKNOWN, null -> "peppy.attachment_storage_unavailable"
}

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

    fun begin(scope: String) {
        if (scope == this.scope) return
        this.scope = scope
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

    private fun AttachmentStorageUiState.lastKnown() = when (this) {
        is AttachmentStorageUiState.Available -> status
        is AttachmentStorageUiState.Unavailable -> lastKnown
        AttachmentStorageUiState.Loading, AttachmentStorageUiState.Unsupported -> null
    }
}
