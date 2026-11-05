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
}

private object GoLinkEngine : LinkEngine {
    override val available = true

    override fun open(state: ByteArray, host: LinkHost): LinkHandle = GoLinkHandle(
        Gmlink.open(state, object : Host {
            override fun persistState(state: ByteArray) = host.persistState(state)
            override fun event(kind: String, code: String, value: Long) = host.event(kind, code, value)
            override fun showEmoji(emoji: String) = host.showEmoji(emoji)
        }),
    )
}

object LinkEngines {
    val current: LinkEngine = GoLinkEngine
}
