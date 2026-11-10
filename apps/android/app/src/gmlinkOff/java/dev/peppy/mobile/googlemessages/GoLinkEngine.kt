package dev.peppy.mobile.googlemessages

private object UnavailableLinkEngine : LinkEngine {
    override val available = false

    override fun open(state: ByteArray, host: LinkHost): LinkHandle =
        throw IllegalStateException("link_unavailable")
}

object LinkEngines {
    val current: LinkEngine = UnavailableLinkEngine
}
