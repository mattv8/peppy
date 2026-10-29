package dev.peppy.mobile

internal sealed interface StoredSecret {
    data class Present(val value: CharArray) : StoredSecret
    data object Missing : StoredSecret
}

internal class SecureStoreUnavailable : Exception()

internal class VaultSecretStore(private val store: HostedSecureStore) {
    fun account(vaultId: String, deviceId: String): String = "vault-secret:v2:$vaultId:$deviceId"
    fun pendingAccount(origin: String): String = "vault-secret:pending:v2:$origin"
    fun pendingAccount(origin: String, accountId: String): String = "vault-secret:pending:v3:$origin:$accountId"

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

    /** Writes only into a missing binding and proves the resulting value by reading it back. */
    fun saveAndReadbackIfMissing(vaultId: String, deviceId: String, secret: CharArray): Boolean = try {
        when (val current = read(vaultId, deviceId)) {
            is StoredSecret.Present -> try {
                current.value.contentEquals(secret)
            } finally {
                current.value.fill('\u0000')
            }
            StoredSecret.Missing -> {
                val bytes = secret.concatToString().toByteArray(Charsets.UTF_8)
                try { store.write(account(vaultId, deviceId), bytes) } finally { bytes.fill(0) }
                when (val written = read(vaultId, deviceId)) {
                    is StoredSecret.Present -> try {
                        written.value.contentEquals(secret)
                    } finally {
                        written.value.fill('\u0000')
                    }
                    StoredSecret.Missing -> false
                }
            }
        }
    } catch (_: Exception) {
        false
    }

    /** Removes only the legacy origin-scoped pending value that equals an already verified secret. */
    fun removeOriginPendingIfMatching(origin: String, secret: CharArray) {
        try {
            when (val pending = readPending(origin)) {
                is StoredSecret.Present -> try {
                    if (pending.value.contentEquals(secret)) store.remove(pendingAccount(origin))
                } finally {
                    pending.value.fill('\u0000')
                }
                StoredSecret.Missing -> Unit
            }
        } catch (_: Exception) {
            Unit
        }
    }

    /** Creation cannot continue unless its recoverable origin-scoped secret is durable. */
    fun savePending(origin: String, secret: CharArray): Boolean = writeIfMissing(pendingAccount(origin), secret)

    fun savePending(origin: String, accountId: String, secret: CharArray): Boolean = writeMatchingOrMissing(pendingAccount(origin, accountId), secret)

    fun readPending(origin: String): StoredSecret = readAccount(pendingAccount(origin))
    fun readPending(origin: String, accountId: String): StoredSecret = readAccount(pendingAccount(origin, accountId))

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

    private fun writeMatchingOrMissing(account: String, secret: CharArray): Boolean = try {
        when (val current = readAccount(account)) {
            is StoredSecret.Present -> try { current.value.contentEquals(secret) } finally { current.value.fill('\u0000') }
            StoredSecret.Missing -> {
                val bytes = secret.concatToString().toByteArray(Charsets.UTF_8)
                try { store.write(account, bytes); true } finally { bytes.fill(0) }
            }
        }
    } catch (_: Exception) { false }

    /**
     * Writes the account's pending secret to a missing binding, verifies it by reading it back, and only then
     * removes the pending slot. An existing different binding is never overwritten and keeps the pending slot.
     * A retry after the pending slot was already removed succeeds only when the binding exists.
     */
    fun movePendingToBinding(origin: String, accountId: String, vaultId: String, deviceId: String): Boolean = try {
        val account = pendingAccount(origin, accountId)
        when (val pending = readAccount(account)) {
            StoredSecret.Missing -> when (val binding = read(vaultId, deviceId)) {
                StoredSecret.Missing -> false
                is StoredSecret.Present -> try { true } finally { binding.value.fill('\u0000') }
            }
            is StoredSecret.Present -> try {
                saveIfMissing(vaultId, deviceId, pending.value)
                val binding = read(vaultId, deviceId)
                val matches = try { bindingMatches(binding, pending.value) } finally {
                    if (binding is StoredSecret.Present) binding.value.fill('\u0000')
                }
                if (matches) store.remove(account)
                matches
            } finally { pending.value.fill('\u0000') }
        }
    } catch (_: Exception) {
        false
    }

    private fun bindingMatches(binding: StoredSecret, pending: CharArray): Boolean =
        binding is StoredSecret.Present && binding.value.contentEquals(pending)
}
