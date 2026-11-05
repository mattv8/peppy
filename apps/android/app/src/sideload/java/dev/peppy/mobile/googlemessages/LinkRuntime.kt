package dev.peppy.mobile.googlemessages

import android.content.Context
import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicBoolean
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow

object LinkRuntime {
    private val callbackExecutor: ExecutorService = Executors.newSingleThreadExecutor()
    private val changeCounter = MutableStateFlow(0L)
    private var cachedHandle: LinkHandle? = null

    val emoji = MutableStateFlow<String?>(null)
    val changes: StateFlow<Long> = changeCounter
    val pairingInProgress = AtomicBoolean(false)

    fun handle(context: Context): LinkOpenResult = synchronized(this) {
        cachedHandle?.let { return LinkOpenResult.Open(it) }
        val engine = LinkRuntimeTestHooks.engine ?: LinkEngines.current
        if (!engine.available) return LinkOpenResult.Unavailable
        val store = store(context)
        val state = try {
            store.read()
        } catch (error: IllegalStateException) {
            return LinkOpenResult.Unreadable(filterReasonCode(error.message))
        }
        return try {
            engine.open(state, host(store)).also { cachedHandle = it }.let(LinkOpenResult::Open)
        } catch (error: IllegalStateException) {
            if (error.message == "link_unavailable") LinkOpenResult.Unavailable
            else LinkOpenResult.Unreadable(filterReasonCode(error.message))
        }
    }

    fun resetLocalState(context: Context) {
        GoogleMessagesLinkService.stop(context)
        synchronized(this) {
            cachedHandle?.close()
            cachedHandle = null
            store(context).wipe()
            emoji.value = null
        }
        recordEvent("link", "reset_local", 0)
        signalChange()
    }

    internal fun resetForTest() = synchronized(this) {
        cachedHandle?.close()
        cachedHandle = null
        emoji.value = null
        changeCounter.value = 0
        pairingInProgress.set(false)
    }

    private fun store(context: Context): LinkStateStorage =
        LinkRuntimeTestHooks.storeFactory?.invoke(context.applicationContext)
            ?: LinkStateStoreAdapter(LinkStateStore(context.applicationContext.noBackupFilesDir))

    private fun host(store: LinkStateStorage) = object : LinkHost {
        override fun persistState(state: ByteArray) = store.write(state)

        override fun event(kind: String, code: String, value: Long) {
            val safeCode = filterReasonCode(code)
            callbackExecutor.execute {
                recordEvent(kind, safeCode, value)
                if (kind == "conn" && safeCode == "ready") GoogleMessagesLinkService.resetBackoff()
                signalChange()
            }
        }

        override fun showEmoji(value: String) {
            callbackExecutor.execute {
                emoji.value = value
                signalChange()
            }
        }
    }

    private fun recordEvent(kind: String, code: String, value: Long) {
        LinkRuntimeTestHooks.eventRecorder?.invoke(kind, code, value)
    }

    private fun signalChange() {
        changeCounter.value += 1
    }
}

internal interface LinkStateStorage {
    fun read(): ByteArray
    fun write(state: ByteArray)
    fun wipe()
}

private class LinkStateStoreAdapter(private val store: LinkStateStore) : LinkStateStorage {
    override fun read(): ByteArray = store.read()
    override fun write(state: ByteArray) = store.write(state)
    override fun wipe() = store.wipe()
}

internal fun filterReasonCode(code: String?): String =
    code?.takeIf { REASON_CODE.matches(it) } ?: "unknown"

private val REASON_CODE = Regex("[a-z][a-z0-9_]{0,39}")

internal object LinkRuntimeTestHooks {
    @Volatile var engine: LinkEngine? = null
    @Volatile var storeFactory: ((Context) -> LinkStateStorage)? = null
    @Volatile var eventRecorder: ((String, String, Long) -> Unit)? = null
}
