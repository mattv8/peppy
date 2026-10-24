package dev.peppy.mobile

import android.content.Context
import android.net.Uri
import kotlinx.coroutines.CancellationException
import java.util.concurrent.atomic.AtomicLong
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.update

internal data class DiagnosticsState(val enabled: Boolean = false, val bytes: Long = 0, val dropped: Long = 0, val busy: Boolean = false, val error: DiagnosticsError? = null)
internal enum class DiagnosticsError { STORAGE, EXPORT, CLEAR }

internal object DiagnosticsController {
    private val mutableState = MutableStateFlow(DiagnosticsState())
    val state: StateFlow<DiagnosticsState> = mutableState
    private var preferences: DiagnosticsPreferences? = null
    private var log: DiagnosticsLog? = null
    @Volatile private var exportUri: Uri? = null
    private val exportGeneration = AtomicLong()
    private val activeOperations = AtomicLong()

    fun initialize(context: Context) {
        if (preferences != null) return
        preferences = DiagnosticsPreferences(context.applicationContext)
        log = DiagnosticsLog(context.applicationContext) { bytes, dropped, error ->
            mutableState.update { state -> state.copy(bytes = bytes, dropped = dropped, error = error ?: state.error) }
        }
        val enabled = preferences!!.enabled()
        mutableState.update { it.copy(enabled = enabled) }
        SyncDiagnostics.configureRuntime(enabled) { line -> log?.record(line) }
        log!!.initialize()
        if (enabled) log!!.setEnabled(true)
    }

    fun setEnabled(enabled: Boolean) {
        val activeLog = log ?: return
        preferences?.setEnabled(enabled)
        exportGeneration.incrementAndGet()
        exportUri = null
        mutableState.update { it.copy(enabled = enabled, error = null) }
        SyncDiagnostics.configureRuntime(enabled) { line -> activeLog.record(line) }
        activeLog.setEnabled(enabled)
    }

    suspend fun export(): Uri? {
        val activeLog = log ?: return null
        val requestGeneration = exportGeneration.get()
        beginOperation()
        return try {
            val uri = activeLog.export()
            if (requestGeneration != exportGeneration.get()) return null
            exportUri = uri
            mutableState.update { it.copy(error = if (uri == null) DiagnosticsError.EXPORT else null) }
            uri
        } catch (error: CancellationException) {
            throw error
        } catch (_: Exception) {
            mutableState.update { it.copy(error = DiagnosticsError.EXPORT) }
            null
        } finally {
            endOperation()
        }
    }

    suspend fun clear() {
        val activeLog = log ?: return
        exportGeneration.incrementAndGet()
        exportUri = null
        beginOperation()
        try {
            activeLog.clear()
        } catch (error: CancellationException) {
            throw error
        } catch (_: Exception) {
            mutableState.update { it.copy(error = DiagnosticsError.CLEAR) }
        } finally {
            endOperation()
        }
    }

    fun isExportCurrent(uri: Uri): Boolean = uri == exportUri

    private fun beginOperation() {
        activeOperations.incrementAndGet()
        mutableState.update { it.copy(busy = true, error = null) }
    }

    private fun endOperation() {
        if (activeOperations.decrementAndGet() == 0L) mutableState.update { it.copy(busy = false) }
    }
}
