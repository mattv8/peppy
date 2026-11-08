package dev.peppy.mobile.googlemessages

import dev.peppy.mobile.telephony.OwnNumberRawObservation
import dev.peppy.mobile.telephony.OwnNumberRawSource
import dev.peppy.mobile.telephony.SimRoute
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class RcsNativeAndroidRouteReaderTest {
    private val route = SimRoute("android-subscription-7", 7, "Default SMS SIM")

    @Test
    fun `returns current route raw value source region and a fresh timestamp`() {
        var clock = 100L
        val reader = RcsNativeAndroidRouteReader(
            currentRoute = { route },
            observeRaw = { OwnNumberRawObservation("+1 (555) 555-0100", OwnNumberRawSource.SubscriptionManagerPhoneNumber) },
            countryIso = { subscriptionId -> assertEquals(7, subscriptionId); "us" },
            wallTimeMs = { ++clock },
        )

        assertEquals(
            RcsNativeRouteEvidence(route.routeId, 7, "+1 (555) 555-0100", RcsNativeOwnNumberSource.SubscriptionManagerPhoneNumber, "US", 101),
            reader.currentRoute(),
        )
        assertEquals(102L, reader.currentRoute()?.observedAtMs)
    }

    @Test
    fun `fails closed for missing invalid or changed default subscription`() {
        assertNull(RcsNativeAndroidRouteReader(currentRoute = { null }, observeRaw = { error("must not read") }, countryIso = { error("must not read") }, wallTimeMs = { error("must not timestamp") }).currentRoute())
        assertNull(RcsNativeAndroidRouteReader(currentRoute = { route.copy(subscriptionId = -1, routeId = "android-subscription--1") }, observeRaw = { error("must not read invalid route") }, countryIso = { error("must not read invalid route") }, wallTimeMs = { error("must not timestamp invalid route") }).currentRoute())
        assertNull(RcsNativeAndroidRouteReader(currentRoute = { route }, observeRaw = { null }, countryIso = { "US" }, wallTimeMs = { error("must not timestamp") }).currentRoute())
        assertNull(RcsNativeAndroidRouteReader(currentRoute = { route }, observeRaw = { OwnNumberRawObservation("+15555550100", OwnNumberRawSource.TelephonyLine1Number) }, countryIso = { "USA" }, wallTimeMs = { error("must not timestamp") }).currentRoute())

        var reads = 0
        val changed = RcsNativeAndroidRouteReader(
            currentRoute = { if (reads++ == 0) route else route.copy(subscriptionId = 8, routeId = "android-subscription-8") },
            observeRaw = { OwnNumberRawObservation("+15555550100", OwnNumberRawSource.TelephonyLine1Number) },
            countryIso = { "US" },
            wallTimeMs = { error("must not timestamp changed route") },
        )
        assertNull(changed.currentRoute())
    }
}
