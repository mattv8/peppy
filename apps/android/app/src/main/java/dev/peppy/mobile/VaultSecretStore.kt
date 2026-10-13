package dev.peppy.mobile

internal sealed interface StoredSecret {
    data class Present(val value: CharArray) : StoredSecret
    data object Missing : StoredSecret
}

internal class SecureStoreUnavailable : Exception()

internal class VaultSecretStore(private val store: HostedSecureStore) {
    fun account(vaultId: String, deviceId: String): String = "vault-secret:v2:$vaultId:$deviceId"
    fun pendingAccount(origin: String): String = "vault-secret:pending:v2:$origin"

    fun read(vaultId: String, deviceId: String): StoredSecret = try {
        val bytes = store.read(account(vaultId, deviceId)) ?: return StoredSecret.Missing
        try {
            if (bytes.isEmpty()) StoredSecret.Missing else StoredSecret.Present(bytes.toString(Charsets.UTF_8).toCharArray())
        } finally {
            bytes.fill(0)
        }
    } catch (_: Exception) {
        throw SecureStoreUnavailable()
    }

    fun saveIfMissing(vaultId: String, deviceId: String, secret: CharArray): Boolean = try {
        when (val current = read(vaultId, deviceId)) {
            is StoredSecret.Present -> try { false } finally { current.value.fill('\u0000') }
            StoredSecret.Missing -> {
                val bytes = secret.concatToString().toByteArray(Charsets.UTF_8)
                try { store.write(account(vaultId, deviceId), bytes); true } finally { bytes.fill(0) }
            }
        }
    } catch (_: Exception) {
        false
    }

    /** Creation cannot continue unless its recoverable origin-scoped secret is durable. */
    fun savePending(origin: String, secret: CharArray): Boolean = writeIfMissing(pendingAccount(origin), secret)

    fun readPending(origin: String): StoredSecret = readAccount(pendingAccount(origin))

    /**
     * Copies pending material to the binding, then tombstones the origin slot so a later vault on this origin
     * cannot adopt it. Pending material is kept when the binding copy cannot be written.
     */
    fun movePendingToBinding(origin: String, vaultId: String, deviceId: String) {
        when (val pending = readPending(origin)) {
            StoredSecret.Missing -> Unit
            is StoredSecret.Present -> try {
                saveIfMissing(vaultId, deviceId, pending.value)
                when (val binding = read(vaultId, deviceId)) {
                    StoredSecret.Missing -> Unit
                    is StoredSecret.Present -> try {
                        if (binding.value.contentEquals(pending.value)) removePending(origin)
                    } finally { binding.value.fill('\u0000') }
                }
            } finally { pending.value.fill('\u0000') }
        }
    }

    /** Best-effort: if removal fails, the pending copy stays, which is the pre-tombstone behaviour. */
    private fun removePending(origin: String) {
        try {
            store.remove(pendingAccount(origin))
        } catch (_: Exception) {
            Unit
        }
    }

    private fun readAccount(account: String): StoredSecret = try {
        val bytes = store.read(account) ?: return StoredSecret.Missing
        try {
            if (bytes.isEmpty()) StoredSecret.Missing else StoredSecret.Present(bytes.toString(Charsets.UTF_8).toCharArray())
        } finally { bytes.fill(0) }
    } catch (_: Exception) { throw SecureStoreUnavailable() }

    private fun writeIfMissing(account: String, secret: CharArray): Boolean = try {
        when (val current = readAccount(account)) {
            is StoredSecret.Present -> try { true } finally { current.value.fill('\u0000') }
            StoredSecret.Missing -> {
                val bytes = secret.concatToString().toByteArray(Charsets.UTF_8)
                try { store.write(account, bytes); true } finally { bytes.fill(0) }
            }
        }
    } catch (_: Exception) { false }
}
