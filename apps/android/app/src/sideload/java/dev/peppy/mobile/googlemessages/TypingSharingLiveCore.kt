package dev.peppy.mobile.googlemessages

import dev.peppy.mobile.googlemessages.live.LiveCore
import dev.peppy.mobile.googlemessages.live.LiveMembershipSnapshot
import dev.peppy.mobile.googlemessages.live.LiveRejected

/** Reapplies the host-owned sharing choice before an enabled core can admit publication. */
internal class TypingSharingLiveCore(
    private val delegate: LiveCore,
    private val preferences: TypingSharingPreferences,
    private val apply: (Boolean) -> Unit,
) : LiveCore {
    override fun enable(snapshot: LiveMembershipSnapshot) {
        delegate.enable(snapshot)
        if (!preferences.reapplyAfterEnable(apply)) {
            runCatching { delegate.disable() }
            throw LiveRejected()
        }
    }

    override fun disable() = delegate.disable()
}
