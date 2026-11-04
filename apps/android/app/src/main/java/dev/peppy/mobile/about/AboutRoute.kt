package dev.peppy.mobile.about

internal sealed interface AboutRoute {
    data object Closed : AboutRoute
    data object About : AboutRoute
    data object Licenses : AboutRoute
    data class License(val fileName: String) : AboutRoute
}
