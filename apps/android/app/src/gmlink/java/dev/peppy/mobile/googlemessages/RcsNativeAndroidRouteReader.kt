package dev.peppy.mobile.googlemessages

import android.content.Context
import android.telephony.SubscriptionManager
import dev.peppy.mobile.telephony.OwnNumberDetector
import dev.peppy.mobile.telephony.OwnNumberRawObservation
import dev.peppy.mobile.telephony.OwnNumberRawSource
import dev.peppy.mobile.telephony.SimRoute
import dev.peppy.mobile.telephony.SimRoutes

internal class RcsNativeAndroidRouteReader(
    currentRoute: () -> SimRoute?,
    private val observeRaw: (SimRoute) -> OwnNumberRawObservation?,
    private val countryIso: (Int) -> String?,
    private val wallTimeMs: () -> Long,
) : RcsNativeRouteReader {
    private val routeProvider = currentRoute
    internal constructor(context: Context, clock: RcsNativeClock) : this(
        currentRoute = { SimRoutes.current().singleOrNull() },
        observeRaw = { route -> OwnNumberDetector.observeRaw(context, route) },
        countryIso = { subscriptionId -> OwnNumberDetector.simCountryIso(context, subscriptionId) },
        wallTimeMs = clock::wallTimeMs,
    )

    override fun currentRoute(): RcsNativeRouteEvidence? {
        val route = routeProvider() ?: return null
        if (route.subscriptionId == SubscriptionManager.INVALID_SUBSCRIPTION_ID || route.subscriptionId < 0) return null
        val observation = observeRaw(route) ?: return null
        val region = countryIso(route.subscriptionId)
            ?.uppercase()
            ?.takeIf { it.matches(Regex("[A-Z]{2}")) }
            ?: return null
        if (routeProvider() != route) return null
        return RcsNativeRouteEvidence(
            routeId = route.routeId,
            subscriptionId = route.subscriptionId,
            ownNumberRaw = observation.value,
            ownNumberSource = observation.source.toRcsSource(),
            identityRegion = region,
            observedAtMs = wallTimeMs(),
        )
    }
}

private fun OwnNumberRawSource.toRcsSource(): RcsNativeOwnNumberSource = when (this) {
    OwnNumberRawSource.SubscriptionManagerPhoneNumber -> RcsNativeOwnNumberSource.SubscriptionManagerPhoneNumber
    OwnNumberRawSource.TelephonyLine1Number -> RcsNativeOwnNumberSource.TelephonyLine1Number
}
