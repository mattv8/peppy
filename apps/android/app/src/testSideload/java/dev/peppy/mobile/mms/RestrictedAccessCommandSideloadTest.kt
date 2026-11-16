package dev.peppy.mobile.mms

import org.junit.Assert.assertTrue
import org.junit.Test

class RestrictedAccessCommandSideloadTest {
    @Test
    fun commandUsesTheActualPackageName() {
        val command = restrictedMessageAppOpCommand("dev.example.peppy")

        assertTrue(command.contains("dev.example.peppy"))
        assertTrue(command.contains(READ_RESTRICTED_MESSAGES_OP))
    }
}
