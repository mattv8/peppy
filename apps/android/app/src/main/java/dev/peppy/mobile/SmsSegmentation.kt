package dev.peppy.mobile

import android.icu.text.BreakIterator

internal object SmsSegmentation {
    const val UCS2_SINGLE_PART_UNITS = 70
    const val UCS2_CONCATENATED_PART_UNITS = 67

    fun divide(body: String, platform: (String) -> List<String>): List<String> = try {
        platform(body)
    } catch (_: SecurityException) {
        ucs2Parts(body)
    }

    fun ucs2Parts(body: String): List<String> {
        if (body.isEmpty()) return emptyList()
        if (body.length <= UCS2_SINGLE_PART_UNITS) return listOf(body)

        val parts = mutableListOf<String>()
        val currentPart = StringBuilder()
        val iterator = BreakIterator.getCharacterInstance().apply { setText(body) }
        var start = iterator.first()
        var end = iterator.next()
        while (end != BreakIterator.DONE) {
            val cluster = body.substring(start, end)
            if (cluster.length > UCS2_CONCATENATED_PART_UNITS) {
                addCurrentPart(parts, currentPart)
                parts += splitOnCodePointBoundaries(cluster)
            } else if (currentPart.length + cluster.length > UCS2_CONCATENATED_PART_UNITS) {
                addCurrentPart(parts, currentPart)
                currentPart.append(cluster)
            } else {
                currentPart.append(cluster)
            }
            start = end
            end = iterator.next()
        }
        addCurrentPart(parts, currentPart)
        return parts
    }

    private fun addCurrentPart(parts: MutableList<String>, currentPart: StringBuilder) {
        if (currentPart.isNotEmpty()) {
            parts += currentPart.toString()
            currentPart.clear()
        }
    }

    private fun splitOnCodePointBoundaries(cluster: String): List<String> {
        val parts = mutableListOf<String>()
        var start = 0
        var index = 0
        var partLength = 0
        while (index < cluster.length) {
            val codePointLength = Character.charCount(cluster.codePointAt(index))
            if (partLength + codePointLength > UCS2_CONCATENATED_PART_UNITS) {
                parts += cluster.substring(start, index)
                start = index
                partLength = 0
            }
            index += codePointLength
            partLength += codePointLength
        }
        parts += cluster.substring(start)
        return parts
    }
}
