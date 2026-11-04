package dev.peppy.mobile.messagingapp

import android.content.ContentResolver
import android.net.Uri
import kotlinx.coroutines.CancellationException

sealed interface ProviderArchiveRead {
    data class Available(val threadIds: Set<Long>, val truncated: Boolean) : ProviderArchiveRead
    data object Unavailable : ProviderArchiveRead
}

/** Bounded, read-only access to the archive state exposed by the default messaging provider. */
class ProviderArchiveReader(private val resolver: ContentResolver) {
    fun archivedThreadIds(): ProviderArchiveRead {
        return try {
            val cursor = resolver.query(CONVERSATIONS, arrayOf(ID), "archived=1", null, null)
                ?: return ProviderArchiveRead.Unavailable
            cursor.use {
                val idColumn = it.getColumnIndex(ID)
                if (idColumn < 0) return@use ProviderArchiveRead.Unavailable
                val threadIds = linkedSetOf<Long>()
                var truncated = false
                var rows = 0
                while (it.moveToNext()) {
                    if (rows == MAX_THREADS) {
                        truncated = true
                        break
                    }
                    rows++
                    if (!it.isNull(idColumn)) threadIds += it.getLong(idColumn)
                }
                ProviderArchiveRead.Available(threadIds, truncated)
            }
        } catch (error: CancellationException) {
            throw error
        } catch (_: SecurityException) {
            ProviderArchiveRead.Unavailable
        } catch (_: RuntimeException) {
            ProviderArchiveRead.Unavailable
        }
    }

    private companion object {
        val CONVERSATIONS: Uri = Uri.parse("content://mms-sms/conversations?simple=true")
        const val ID = "_id"
        const val MAX_THREADS = 10_000
    }
}
