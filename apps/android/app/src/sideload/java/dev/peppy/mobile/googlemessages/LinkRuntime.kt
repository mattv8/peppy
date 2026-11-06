package dev.peppy.mobile.googlemessages

import android.content.Context
import dev.peppy.mobile.diagnostics.recordGoogleMessagesLinkEvent
import java.util.concurrent.Executor
import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicBoolean
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import uniffi.peppy_mobile_bindings.NativeTypingState

internal data class LinkHandleLease(val handle: LinkHandle, val generation: Long)

object LinkRuntime {
    private val callbackExecutor: ExecutorService = Executors.newSingleThreadExecutor()
    private val nativeOperationLock = Any()
    private val changeCounter = MutableStateFlow(0L)
    private var cachedHandle: LinkHandle? = null
    private var closingHandle: LinkHandle? = null
    private var handleGeneration = 0L
    private var pairingGeneration = 0L

    val emoji = MutableStateFlow<String?>(null)
    val changes: StateFlow<Long> = changeCounter
    val pairingInProgress = AtomicBoolean(false)

    fun handle(context: Context): LinkOpenResult {
        while (true) {
            awaitNoClosingHandle()
            LinkRuntimeTestHooks.afterClosingCheck?.invoke()
            val opened = synchronized(nativeOperationLock) {
                val closing = synchronized(this) {
                    cachedHandle?.let { return LinkOpenResult.Open(it) }
                    closingHandle != null
                }
                if (closing) null else {
                    val engine = LinkRuntimeTestHooks.engine ?: LinkEngines.current
                    if (!engine.available) LinkOpenResult.Unavailable else {
                        val store = store(context)
                        val state = try { store.read() } catch (error: Exception) {
                            return@synchronized LinkOpenResult.Unreadable(filterReasonCode(error.message))
                        }
                        try {
                            val handle = engine.open(state, host(store, nextHandleGeneration()))
                            synchronized(this) {
                                handleGeneration += 1
                                cachedHandle = handle
                            }
                            LinkOpenResult.Open(handle)
                        } catch (error: Exception) {
                            if (error.message == "link_unavailable") LinkOpenResult.Unavailable
                            else LinkOpenResult.Unreadable(filterReasonCode(error.message))
                        }
                    }
                }
            }
            if (opened != null) return opened
        }
    }

    fun resetLocalState(context: Context) {
        GoogleMessagesLinkService.stopAndAwait(context)
        awaitNoClosingHandle()
        synchronized(nativeOperationLock) {
            val handle = synchronized(this) {
                val current = cachedHandle
                cachedHandle = null
                handleGeneration += 1
                emoji.value = null
                pairingInProgress.set(false)
                pairingGeneration += 1
                current
            }
            handle?.close()
            store(context).wipe()
            if (!recordUnlinked(context)) throw IllegalStateException("persist_failed")
        }
        recordEvent("link", "reset_local", 0)
        signalChange()
    }

    fun recordPreStart(context: Context): Boolean =
        LinkCoreBridge.recordPreStart(context.applicationContext)

    fun recordUnlinked(context: Context): Boolean =
        LinkCoreBridge.recordIfNeeded(context.applicationContext, LinkStatus(0, "unlinked", false, false, false, 0, ""))

    internal fun handleLease(context: Context): LinkHandleLease? {
        val opened = (handle(context) as? LinkOpenResult.Open)?.handle ?: return null
        return synchronized(this) {
            opened.takeIf { it === cachedHandle }?.let { LinkHandleLease(it, handleGeneration) }
        }
    }

    /** Forwards an already-admitted typing effect without adding another asynchronous queue. */
    internal fun setTyping(context: Context, intent: LiveTypingEffectIntent): String {
        val lease = handleLease(context) ?: return "typing_unavailable"
        return setTyping(lease, intent)
    }

    internal fun setTyping(lease: LinkHandleLease, intent: LiveTypingEffectIntent): String = synchronized(nativeOperationLock) {
        if (!ownsLease(lease)) return@synchronized "typing_stale"
        if (!intent.isCurrent.isCurrent()) return@synchronized "typing_stale"
        val ownerGeneration = lease.handle.ownerGeneration()
        if (ownerGeneration <= 0) return@synchronized "typing_stale"
        val request = typingRequest(intent, ownerGeneration) ?: return@synchronized "typing_expired"
        try {
            lease.handle.setTyping(request)
        } catch (_: Throwable) {
            "typing_failed"
        }
    }

    /** Detaches the captured handle only while [lease] owns it. Native close is always outside the runtime monitor. */
    internal fun detachHandleForServiceStop(lease: LinkHandleLease): LinkHandle? = synchronized(nativeOperationLock) {
        synchronized(this) {
            if (lease.generation != handleGeneration || lease.handle !== cachedHandle) null else {
                cachedHandle = null
                closingHandle = lease.handle
                handleGeneration += 1
                lease.handle
            }
        }
    }

    internal fun closeHandleForServiceStop(lease: LinkHandleLease) {
        detachHandleForServiceStop(lease)?.let(::closeDetachedHandle)
    }

    internal fun closeDetachedHandle(handle: LinkHandle) = synchronized(nativeOperationLock) {
        try {
            handle.close()
        } finally {
            synchronized(this) {
                if (closingHandle === handle) {
                    closingHandle = null
                    (this@LinkRuntime as java.lang.Object).notifyAll()
                }
            }
        }
    }

    /** Claims one owner-requested Pair or Relink attempt, returning its opaque generation token. */
    fun tryBeginPairing(): Long? = synchronized(this) {
        if (!pairingInProgress.compareAndSet(false, true)) return null
        pairingGeneration += 1
        emoji.value = null
        signalChange()
        pairingGeneration
    }

    /** Completes exactly the attempt that acquired [attempt]; stale finally blocks are inert. */
    fun endPairing(attempt: Long): Boolean = synchronized(this) {
        if (!pairingInProgress.get() || attempt != pairingGeneration) return false
        pairingInProgress.set(false)
        pairingGeneration += 1
        emoji.value = null
        signalChange()
        true
    }

    /** Compatibility helper for callers that cannot yet retain an attempt token. */
    internal fun endPairing(): Boolean = synchronized(this) {
        if (!pairingInProgress.get()) return false
        endPairing(pairingGeneration)
    }

    internal fun resetForTest() {
        GoogleMessagesLinkService.stopActiveWorker()
        awaitNoClosingHandle()
        synchronized(nativeOperationLock) {
            val captured = synchronized(this) {
                val current = cachedHandle
                cachedHandle = null
                handleGeneration += 1
                emoji.value = null
                changeCounter.value = 0
                pairingInProgress.set(false)
                current
            }
            captured?.close()
        }
    }

    private fun store(context: Context): LinkStateStorage =
        LinkRuntimeTestHooks.storeFactory?.invoke(context.applicationContext)
            ?: LinkStateStoreAdapter(LinkStateStore(context.applicationContext.noBackupFilesDir))

    private fun host(store: LinkStateStorage, generation: Long) = object : LinkHost {
        override fun persistState(state: ByteArray) = store.write(state)

        override fun event(kind: String, code: String, value: Long) = eventOwned(kind, code, value, -1, -1)

        override fun eventOwned(kind: String, code: String, value: Long, ownerGeneration: Long, ownerRevision: Long) {
            val safeCode = filterReasonCode(code)
            callbackExecutor().execute {
                if (!acceptsCallback(generation, ownerGeneration, ownerRevision)) return@execute
                recordEvent(kind, safeCode, value)
                GoogleMessagesLinkService.onLinkEvent(kind, safeCode)
                signalChange()
            }
        }

        override fun rcsEvent(recordJson: String) = rcsEventOwned(recordJson, -1, -1, -1)

        override fun rcsEventOwned(recordJson: String, ownerGeneration: Long, ownerRevision: Long, streamEpoch: Long) {
            callbackExecutor().execute {
                if (!acceptsStreamCallback(generation, ownerGeneration, ownerRevision, streamEpoch)) return@execute
                (LinkRuntimeTestHooks.rcsReceiveSink ?: NoOpRcsReceiveSink).receive(recordJson)
            }
        }

        override fun typingEvent(recordJson: String) = typingEventOwned(recordJson, -1, -1, -1)

        override fun typingEventOwned(recordJson: String, ownerGeneration: Long, ownerRevision: Long, streamEpoch: Long) {
            callbackExecutor().execute {
                if (!acceptsStreamCallback(generation, ownerGeneration, ownerRevision, streamEpoch)) return@execute
                (LinkRuntimeTestHooks.typingReceiveSink ?: NoOpTypingReceiveSink).receive(recordJson)
            }
        }

        override fun showEmoji(value: String) = showEmojiOwned(value, -1, -1)

        override fun showEmojiOwned(value: String, ownerGeneration: Long, ownerRevision: Long) {
            val callbackPairingGeneration = synchronized(this@LinkRuntime) { pairingGeneration }
            callbackExecutor().execute {
                if (!acceptsCallback(generation, ownerGeneration, ownerRevision)) return@execute
                assignEmojiIfCurrent(generation, callbackPairingGeneration, value)
            }
        }
    }

    private fun recordEvent(kind: String, code: String, value: Long) {
        LinkRuntimeEventSink.record(kind, code, value)
    }

    private fun signalChange() {
        changeCounter.value += 1
    }

    private fun acceptsCallback(handle: Long, ownerGeneration: Long, ownerRevision: Long): Boolean = synchronized(this) {
        if (handle != handleGeneration) return@synchronized false
        val current = cachedHandle ?: return@synchronized false
        ownerGeneration >= 0 && ownerRevision >= 0 && current.acceptsOutput(ownerGeneration, ownerRevision)
    }

    private fun acceptsStreamCallback(handle: Long, ownerGeneration: Long, ownerRevision: Long, streamEpoch: Long): Boolean = synchronized(this) {
        if (handle != handleGeneration) return@synchronized false
        val current = cachedHandle ?: return@synchronized false
        ownerGeneration >= 0 && ownerRevision >= 0 && streamEpoch >= 0 &&
            current.acceptsStreamOutput(ownerGeneration, ownerRevision, streamEpoch)
    }

    private fun assignEmojiIfCurrent(handle: Long, attempt: Long, value: String) = synchronized(this) {
        if (handle != handleGeneration || cachedHandle == null) return@synchronized
        if (!pairingInProgress.get() || attempt != pairingGeneration) return@synchronized
        emoji.value = value
        signalChange()
    }

    private fun callbackExecutor(): Executor = LinkRuntimeTestHooks.callbackExecutor ?: callbackExecutor

    private fun ownsLease(lease: LinkHandleLease): Boolean = synchronized(this) {
        lease.generation == handleGeneration && lease.handle === cachedHandle
    }

    private fun nextHandleGeneration(): Long = synchronized(this) { handleGeneration + 1 }

    private fun awaitNoClosingHandle() = synchronized(this) {
        while (closingHandle != null) (this@LinkRuntime as java.lang.Object).wait()
    }
}

private fun typingRequest(intent: LiveTypingEffectIntent, linkGeneration: Long): LinkTypingRequest? {
    if (intent.state != NativeTypingState.STARTED || linkGeneration < 1) return null
    val now = intent.clock.nowMs()
    if (intent.deadlineMono <= now) return null
    val remainingValidityMs = minOf(intent.deadlineMono - now, MAX_TYPING_VALIDITY_MS)
    return LinkTypingRequest(intent.conversationId, NativeTypingState.STARTED, remainingValidityMs, linkGeneration)
}

private const val MAX_TYPING_VALIDITY_MS = 5_000UL

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
    @Volatile var rcsReceiveSink: RcsReceiveSink? = null
    @Volatile var typingReceiveSink: TypingReceiveSink? = null
    @Volatile var callbackExecutor: Executor? = null
    @Volatile var afterClosingCheck: (() -> Unit)? = null
}

internal object LinkRuntimeEventSink {
    fun record(kind: String, code: String, value: Long) {
        recordGoogleMessagesLinkEvent(kind, code, value)
        LinkRuntimeTestHooks.eventRecorder?.invoke(kind, code, value)
    }
}
