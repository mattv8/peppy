package dev.peppy.mobile.security

import dev.peppy.mobile.gateway.TEST_TOKEN
import dev.peppy.mobile.gateway.credentialJson
import java.util.UUID
import org.junit.Assert.*
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config


@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class CredentialParserTest {
    private val vault = UUID.randomUUID().toString()
    private val device = UUID.randomUUID().toString()

    private fun parse(json: String, debug: Boolean = false) = CredentialParser.parse(json.toByteArray(), allowDebugLoopback = debug)

    @Test
    fun acceptsStrictV1AndCanonicalizesIdentity() {
        val parsed = parse(credentialJson("https://Vault.Example/", vault.uppercase(), device))
        assertNotNull(parsed)
        parsed!!
        assertEquals("https://vault.example", parsed.origin)
        assertEquals(vault, parsed.vaultId)
        assertEquals(device, parsed.deviceId)
        assertEquals(false, parsed.toString().contains(TEST_TOKEN))
    }

    @Test
    fun rejectsMalformedOrUnsafeCredentials() {
        val rejected = listOf(
            credentialJson(vaultId = vault, deviceId = device, extra = ""","extra":"x""""),
            credentialJson(vaultId = vault, deviceId = device).replace("\"version\":1", "\"version\":2"),
            credentialJson(vaultId = vault, deviceId = device).replace("\"version\":1", "\"version\":\"1\""),
            credentialJson(vaultId = vault, deviceId = device, token = "ab".repeat(47)),
            credentialJson(vaultId = vault, deviceId = device, token = "zz".repeat(48)),
            credentialJson(origin = "http://vault.example", vaultId = vault, deviceId = device),
            credentialJson(origin = "https://vault.example/path", vaultId = vault, deviceId = device),
            credentialJson(origin = "https://user@vault.example", vaultId = vault, deviceId = device),
            credentialJson(origin = "https://vault.example?q=1", vaultId = vault, deviceId = device),
            credentialJson(vaultId = "1-1-1-1-1", deviceId = device),
            credentialJson(vaultId = vault, deviceId = "not-a-uuid"),
            "not json",
            credentialJson(vaultId = vault, deviceId = device) + " ".repeat(CredentialParser.MAX_BYTES),
        )
        rejected.forEach { assertNull(it.take(120), parse(it)) }
    }

    private val fingerprint = "a".repeat(64)

    private fun v2(floor: String = "2", fingerprint: String = this.fingerprint, extra: String = "") = credentialJson(
        vaultId = vault,
        deviceId = device,
        extra = ",\"epochFloor\":$floor,\"profileFingerprint\":\"$fingerprint\"$extra",
    ).replace("\"version\":1", "\"version\":2")

    @Test
    fun acceptsStrictV2AnchorAndKeepsV1Unanchored() {
        val anchored = parse(v2())
        assertNotNull(anchored)
        assertEquals(2u, anchored!!.anchor?.epochFloor)
        assertEquals(fingerprint, anchored.anchor?.profileFingerprint)
        assertNull(parse(credentialJson(vaultId = vault, deviceId = device))!!.anchor)
    }

    @Test
    fun rejectsMalformedV2Anchors() {
        val rejected = listOf(
            // Version 2 needs both anchor fields, and version 1 must not carry them.
            credentialJson(vaultId = vault, deviceId = device).replace("\"version\":1", "\"version\":2"),
            credentialJson(vaultId = vault, deviceId = device, extra = ",\"epochFloor\":2,\"profileFingerprint\":\"$fingerprint\""),
            v2(floor = "0"),
            v2(floor = "\"2\""),
            v2(floor = "2.0"),
            v2(floor = "-1"),
            v2(floor = "4294967296"),
            v2(fingerprint = "A".repeat(64)),
            v2(fingerprint = "a".repeat(63)),
            v2(fingerprint = "g".repeat(64)),
            v2(extra = ",\"extra\":\"x\""),
        )
        rejected.forEach { assertNull(it.take(160), parse(it)) }
        assertNotNull(parse(v2(floor = "4294967295")))
    }

    @Test
    fun cleartextOnlyForDebugLoopback() {
        assertNull(parse(credentialJson(origin = "http://10.0.2.2:8080", vaultId = vault, deviceId = device), debug = false))
        assertEquals("http://10.0.2.2:8080", parse(credentialJson(origin = "http://10.0.2.2:8080", vaultId = vault, deviceId = device), debug = true)?.origin)
        assertEquals("http://[::1]:8080", parse(credentialJson(origin = "http://[::1]:8080", vaultId = vault, deviceId = device), debug = true)?.origin)
        assertNull(parse(credentialJson(origin = "http://192.168.1.2", vaultId = vault, deviceId = device), debug = true))
    }
}
