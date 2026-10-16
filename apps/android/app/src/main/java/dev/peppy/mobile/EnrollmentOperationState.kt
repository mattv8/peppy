package dev.peppy.mobile

/** Serializes visible enrollment work and rejects callbacks from invalidated screens. */
internal class EnrollmentOperationState {
    private var generation = 0L
    private var active: Long? = null

    fun begin(): Long? {
        if (active != null) return null
        generation += 1
        return generation.also { active = it }
    }

    fun complete(token: Long): Boolean {
        if (active != token) return false
        active = null
        return true
    }

    fun snapshot(): Long = generation

    fun isCurrentSnapshot(token: Long): Boolean = active == null && generation == token

    fun isActive(token: Long): Boolean = active == token

    fun invalidate() {
        generation += 1
        active = null
    }

    fun isBusy(): Boolean = active != null
}

/** Tracks only whether a backgrounded screen may present a newly revealed secret. */
internal class RevealVisibilityState {
    private val operations = EnrollmentOperationState()

    fun begin(): Long? = operations.begin()

    fun isVisible(token: Long): Boolean = operations.isActive(token)

    fun finish(token: Long) {
        operations.complete(token)
    }

    fun hide() {
        operations.invalidate()
    }
}
