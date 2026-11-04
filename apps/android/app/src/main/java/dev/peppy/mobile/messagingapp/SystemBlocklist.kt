package dev.peppy.mobile.messagingapp

import android.content.Context
import android.provider.BlockedNumberContract
import kotlinx.coroutines.CancellationException

data class BlocklistEntry(val original: String, val e164: String?)

sealed interface BlocklistRead {
    data class Entries(val entries: List<BlocklistEntry>, val truncated: Boolean) : BlocklistRead
    data object NotAllowed : BlocklistRead
    data object Unavailable : BlocklistRead
}

enum class UnblockOutcome { Unblocked, NotBlocked, NotAllowed, Failed }

/** Read and reconcile the system blocked-number list when its separately granted AppOp is available. */
class SystemBlocklist(
    private val context: Context,
    private val access: () -> Boolean,
    private val isBlocked: (String) -> Boolean = { BlockedNumberContract.isBlocked(context, it) },
    private val unblockNumber: (String) -> Unit = { BlockedNumberContract.unblock(context, it) },
) {
    fun read(): BlocklistRead {
        if (!access()) return BlocklistRead.NotAllowed
        return try {
            val cursor = context.contentResolver.query(
                BlockedNumberContract.BlockedNumbers.CONTENT_URI,
                COLUMNS,
                null,
                null,
                null,
            ) ?: return BlocklistRead.Unavailable
            cursor.use {
                val originalColumn = it.getColumnIndex(BlockedNumberContract.BlockedNumbers.COLUMN_ORIGINAL_NUMBER)
                val e164Column = it.getColumnIndex(BlockedNumberContract.BlockedNumbers.COLUMN_E164_NUMBER)
                val entries = mutableListOf<BlocklistEntry>()
                var truncated = false
                while (it.moveToNext()) {
                    if (entries.size == MAX_ENTRIES) {
                        truncated = true
                        break
                    }
                    val original = it.stringAt(originalColumn)?.normalized() ?: continue
                    if (original.toByteArray(Charsets.UTF_8).size > MAX_NUMBER_BYTES) continue
                    val e164 = it.stringAt(e164Column)?.normalized()?.takeIf {
                        it.toByteArray(Charsets.UTF_8).size <= MAX_NUMBER_BYTES
                    }
                    entries += BlocklistEntry(original, e164)
                }
                BlocklistRead.Entries(entries, truncated)
            }
        } catch (error: CancellationException) {
            throw error
        } catch (_: SecurityException) {
            BlocklistRead.NotAllowed
        } catch (_: RuntimeException) {
            BlocklistRead.Unavailable
        }
    }

    fun unblock(number: String): UnblockOutcome {
        if (!access()) return UnblockOutcome.NotAllowed
        return try {
            if (!isBlocked(number)) return UnblockOutcome.NotBlocked
            unblockNumber(number)
            if (isBlocked(number)) UnblockOutcome.Failed else UnblockOutcome.Unblocked
        } catch (_: SecurityException) {
            UnblockOutcome.NotAllowed
        } catch (_: RuntimeException) {
            UnblockOutcome.Failed
        }
    }

    private fun android.database.Cursor.stringAt(column: Int): String? =
        column.takeIf { it >= 0 && !isNull(it) }?.let(::getString)

    private fun String.normalized(): String? = trim().takeIf(String::isNotEmpty)

    private companion object {
        val COLUMNS = arrayOf(
            BlockedNumberContract.BlockedNumbers.COLUMN_ORIGINAL_NUMBER,
            BlockedNumberContract.BlockedNumbers.COLUMN_E164_NUMBER,
        )
        const val MAX_ENTRIES = 5_000
        const val MAX_NUMBER_BYTES = 64
    }
}
