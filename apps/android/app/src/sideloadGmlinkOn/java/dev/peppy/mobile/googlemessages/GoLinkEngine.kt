package dev.peppy.mobile.googlemessages

import dev.peppy.gmlink.gmlink.Gmlink
import dev.peppy.gmlink.gmlink.Host
import dev.peppy.gmlink.gmlink.Link

private class GoLinkHandle(private val link: Link) : LinkHandle {
    override fun pair(cookiesJson: ByteArray) = link.pair(cookiesJson)
    override fun relink() = link.relink()
    override fun run() = link.run()
    override fun checkHealth(): Long = link.checkHealth()
    override fun unlink() = link.unlink()
    override fun stop() = link.stop()
    override fun close() = link.close()
    override fun status(): String = link.status()
    override fun ownerGeneration(): Long = link.ownerGeneration()
    override fun setTyping(request: LinkTypingRequest): String = link.setTyping(request.toJson())
    override fun acceptsOutput(generation: Long, revision: Long): Boolean = link.acceptsOutput(generation, revision)
    override fun acceptsStreamOutput(generation: Long, revision: Long, streamEpoch: Long): Boolean =
        link.acceptsStreamOutput(generation, revision, streamEpoch)
    override fun prepareNativeRcs(requestJson: String): String = link.prepareNativeRcs(requestJson)
    override fun bindNativeRcsRoute(ticketId: String, linkGeneration: Long, sessionGeneration: Long, contextRevision: Long, mappingRevision: Long, observedAtMs: Long): String =
        link.bindNativeRcsRoute(ticketId, linkGeneration, sessionGeneration, contextRevision, mappingRevision, observedAtMs)
    override fun bindNativeRcsTarget(ticketId: String, googleConversationId: String, snapshotToken: String): String = link.bindNativeRcsTarget(ticketId, googleConversationId, snapshotToken)
    override fun bindNativeRcsFacts(ticketId: String, factsJson: String): String = link.bindNativeRcsFacts(ticketId, factsJson)
    override fun commitNativeRcs(ticketId: String, commitJson: String): String = link.commitNativeRcs(ticketId, commitJson)
    override fun revokeNativeSend(ticketId: String): String = link.revokeNativeSend(ticketId)
}

private object GoLinkEngine : LinkEngine {
    override val available = true

    override fun open(restoredState: ByteArray, linkHost: LinkHost): LinkHandle = GoLinkHandle(
        Gmlink.open(restoredState, object : Host {
            // Gomobile invokes these from Go goroutine threads. Never call Link synchronously from a callback.
            override fun persistState(persistedState: ByteArray) {
                try {
                    linkHost.persistState(persistedState)
                } catch (_: Throwable) {
                    // Gomobile turns this checked exception into the Go Host error result.
                    throw Exception("persist_failed")
                }
            }

            override fun event(kind: String, code: String, value: Long, generation: Long, revision: Long) {
                try {
                    linkHost.eventOwned(kind, code, value, generation, revision)
                } catch (_: Throwable) {
                    // Host event delivery must not unwind through JNI.
                }
            }

            override fun rcsEvent(recordJson: String, generation: Long, revision: Long, streamEpoch: Long) {
                try {
                    linkHost.rcsEventOwned(recordJson, generation, revision, streamEpoch)
                } catch (_: Throwable) {
                    // Host receive delivery must not unwind through JNI.
                }
            }

            override fun typingEvent(recordJson: String, generation: Long, revision: Long, streamEpoch: Long) {
                try {
                    linkHost.typingEventOwned(recordJson, generation, revision, streamEpoch)
                } catch (_: Throwable) {
                    // Host typing delivery must not unwind through JNI.
                }
            }

            override fun showEmoji(emoji: String, generation: Long, revision: Long) {
                try {
                    linkHost.showEmojiOwned(emoji, generation, revision)
                } catch (_: Throwable) {
                    // Host display updates must not unwind through JNI.
                }
            }
        }),
    )
}

object LinkEngines {
    val current: LinkEngine = GoLinkEngine
}
