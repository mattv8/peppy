package dev.peppy.mobile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class SmsSegmentationTest {
    private val family = "\uD83D\uDC68\u200D\uD83D\uDC69\u200D\uD83D\uDC67\u200D\uD83D\uDC66" // 👨‍👩‍👧‍👦, 11 units
    private val usFlag = "\uD83C\uDDFA\uD83C\uDDF8" // 🇺🇸, 4 units
    private val thumbsUpMediumDark = "\uD83D\uDC4D\uD83C\uDFFD" // 👍🏽, 4 units
    private val grinning = "\uD83D\uDE00" // 😀
    private val partyPopper = "\uD83C\uDF89" // 🎉
    private val rightSingleQuote = "\u2019"
    private val ellipsis = "\u2026"

    @Test
    fun shortEmojiTextIsOneExactPart() {
        val body = "hi $grinning $partyPopper"

        assertEquals(listOf(body), SmsSegmentation.ucs2Parts(body))
    }

    @Test
    fun originalEquivalentCasesSplitWithinPartLimit() {
        val twoParts = "a".repeat(116) + ellipsis + rightSingleQuote // 118 units
        val eightParts = "a".repeat(482) + rightSingleQuote // 483 units

        assertExactUcs2Parts(twoParts, SmsSegmentation.ucs2Parts(twoParts), expectedCount = 2)
        assertExactUcs2Parts(eightParts, SmsSegmentation.ucs2Parts(eightParts), expectedCount = 8)
    }

    @Test
    fun familyZwjEmojiStraddlingBoundaryStaysIntact() {
        val body = "a".repeat(62) + family + "b".repeat(20) // family spans indexes 62..72, crosses 67

        val parts = SmsSegmentation.ucs2Parts(body)

        assertExactUcs2Parts(body, parts)
        assertTrue(parts.any { it.contains(family) })
    }

    @Test
    fun flagAndSkinToneEmojiStraddlingBoundaryStayIntact() {
        val flagBody = "a".repeat(64) + usFlag + "b".repeat(10) // flag spans 64..67, crosses 67
        val skinToneBody = "a".repeat(66) + thumbsUpMediumDark + "c".repeat(10) // spans 66..69, crosses 67

        val flagParts = SmsSegmentation.ucs2Parts(flagBody)
        val skinToneParts = SmsSegmentation.ucs2Parts(skinToneBody)

        assertExactUcs2Parts(flagBody, flagParts)
        assertTrue(flagParts.any { it.contains(usFlag) })
        assertExactUcs2Parts(skinToneBody, skinToneParts)
        assertTrue(skinToneParts.any { it.contains(thumbsUpMediumDark) })
    }

    @Test
    fun longEmojiOnlyMessageNeverSplitsSurrogatePairs() {
        val body = (partyPopper + thumbsUpMediumDark + usFlag + family + grinning).repeat(10)

        val parts = SmsSegmentation.ucs2Parts(body)

        assertExactUcs2Parts(body, parts)
    }

    @Test
    fun oversizedGraphemeSplitsOnCodePointBoundaries() {
        val body = "e" + "\u0301".repeat(100) // one grapheme of 101 units

        val parts = SmsSegmentation.ucs2Parts(body)

        assertExactUcs2Parts(body, parts)
    }

    @Test
    fun partLimitsMatchUcs2SingleAndConcatenatedPayloads() {
        assertEquals(listOf(70), SmsSegmentation.ucs2Parts("a".repeat(70)).map { it.length })
        assertEquals(listOf(67, 4), SmsSegmentation.ucs2Parts("a".repeat(71)).map { it.length })
        assertEquals(listOf(67, 67), SmsSegmentation.ucs2Parts("a".repeat(134)).map { it.length })
        assertEquals(listOf(67, 67, 1), SmsSegmentation.ucs2Parts("a".repeat(135)).map { it.length })
    }

    @Test
    fun oversizedGraphemeAfterTextKeepsPartOrder() {
        val prefix = "a".repeat(10)
        val cluster = "e" + "\u0301".repeat(100)
        val suffix = "b".repeat(5)

        val parts = SmsSegmentation.ucs2Parts(prefix + cluster + suffix)

        assertEquals(listOf(prefix, cluster.substring(0, 67), cluster.substring(67), suffix), parts)
    }

    @Test
    fun divideReturnsPlatformResultWhenNoException() {
        val body = "a".repeat(116) + ellipsis + rightSingleQuote
        val platformParts = listOf("platform-one", "platform-two")

        val result = SmsSegmentation.divide(body) { platformParts }

        assertEquals(platformParts, result)
    }

    @Test
    fun divideFallsBackToUcs2PartsOnSecurityException() {
        val body = "a".repeat(116) + ellipsis + rightSingleQuote

        val result = SmsSegmentation.divide(body) { throw SecurityException("READ_PHONE_STATE") }

        assertEquals(SmsSegmentation.ucs2Parts(body), result)
        assertExactUcs2Parts(body, result, expectedCount = 2)
    }

    @Test
    fun dividePropagatesIllegalStateException() {
        val body = "a".repeat(116) + ellipsis + rightSingleQuote

        assertThrows(IllegalStateException::class.java) {
            SmsSegmentation.divide(body) { throw IllegalStateException("unavailable") }
        }
    }

    private fun assertExactUcs2Parts(input: String, parts: List<String>, expectedCount: Int? = null) {
        assertEquals(input, parts.joinToString(""))
        if (input.length > SmsSegmentation.UCS2_SINGLE_PART_UNITS) {
            assertTrue("input over 70 units must split", parts.size > 1)
        }
        if (expectedCount != null) {
            assertEquals(expectedCount, parts.size)
        }
        if (parts.size > 1) {
            parts.forEach { part ->
                assertTrue("part exceeds 67 units: ${part.length}", part.length <= SmsSegmentation.UCS2_CONCATENATED_PART_UNITS)
            }
        }
        parts.forEach { part ->
            if (part.isNotEmpty()) {
                assertTrue("part begins with low surrogate", !part.first().isLowSurrogate())
                assertTrue("part ends with high surrogate", !part.last().isHighSurrogate())
            }
        }
    }
}
