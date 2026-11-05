package dev.peppy.mobile.messagingapp

import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import java.io.FileInputStream
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Assume.assumeTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class SystemBlocklistAccessTest {
    private val instrumentation = InstrumentationRegistry.getInstrumentation()
    private val packageName = instrumentation.targetContext.packageName
    private val blocklist = SystemBlocklist(instrumentation.targetContext, { true })
    private var previousWriteSmsMode = DEFAULT_APP_OP_MODE

    @Before fun recordPreviousWriteSmsMode() {
        previousWriteSmsMode = runCatching { parseAppOpMode(shell("appops get $packageName WRITE_SMS")) }
            .getOrDefault(DEFAULT_APP_OP_MODE)
    }

    @After fun cleanUp() {
        runCatching {
            shell("content delete --uri content://com.android.blockednumber/blocked --where \"original_number='+12025550123'\"")
        }
        runCatching { shell("appops set $packageName WRITE_SMS $previousWriteSmsMode") }
    }

    @Test fun writeSmsAppOpAllowsBlocklistReadAndConfirmedUnblock() {
        shell("appops set $packageName WRITE_SMS allow")
        val provider = shell("content query --uri content://com.android.blockednumber/blocked")
        assumeTrue("Blocked-number provider is unavailable: $provider", !provider.indicatesMissingProvider())

        shell("content delete --uri content://com.android.blockednumber/blocked --where \"original_number='+12025550123'\"")
        val inserted = shell(
            "content insert --uri content://com.android.blockednumber/blocked --bind original_number:s:+12025550123",
        )
        assumeTrue("Could not insert blocked-number test row: $inserted", !inserted.indicatesShellFailure())

        val entries = blocklist.read() as? BlocklistRead.Entries
        assertTrue(entries?.entries?.any { it.original == "+12025550123" } == true)
        assertEquals(UnblockOutcome.Unblocked, blocklist.unblock("+12025550123"))
        val afterUnblock = blocklist.read() as? BlocklistRead.Entries
        assertTrue(afterUnblock?.entries?.none { it.original == "+12025550123" } == true)

        shell("appops set $packageName WRITE_SMS default")
        assertEquals(BlocklistRead.NotAllowed, blocklist.read())
        ProviderArchiveReader(instrumentation.targetContext.contentResolver).archivedThreadIds()
    }

    private fun shell(command: String): String = instrumentation.uiAutomation.executeShellCommand(command).use { descriptor ->
        FileInputStream(descriptor.fileDescriptor).bufferedReader().use { it.readText() }
    }

    // `appops get` prints "WRITE_SMS: <mode>; ..." or "No operations." when the op is at its default.
    private fun parseAppOpMode(output: String): String =
        output.lineSequence()
            .firstOrNull { it.startsWith("WRITE_SMS:") }
            ?.removePrefix("WRITE_SMS:")
            ?.substringBefore(';')
            ?.trim()
            ?.takeIf(String::isNotEmpty)
            ?: DEFAULT_APP_OP_MODE

    private fun String.indicatesMissingProvider(): Boolean =
        contains("Unknown URI", ignoreCase = true) ||
            contains("No such provider", ignoreCase = true) ||
            contains("No content provider", ignoreCase = true)

    private fun String.indicatesShellFailure(): Boolean =
        contains("Error", ignoreCase = true) || contains("Exception", ignoreCase = true)

    private companion object {
        const val DEFAULT_APP_OP_MODE = "default"
    }
}
