package dev.peppy.mobile

import android.content.Context
import androidx.annotation.VisibleForTesting

/** Serializes protected hosted checkpoint and pending-secret transitions across client instances. */
internal object HostedProvisioningStorage {
    private val lock = Any()

    @VisibleForTesting
    @Volatile
    internal var storeFactory: (Context) -> HostedSecureStore = ::AndroidHostedSecureStore

    fun store(context: Context): HostedSecureStore = storeFactory(context)

    fun readCheckpoint(store: HostedSecureStore): ByteArray? = synchronized(lock) {
        store.read(HostedAccountClient.CHECKPOINT_KEY)
    }

    fun candidate(
        store: HostedSecureStore,
        checkpoint: ByteArray,
        origin: String,
        accountId: String,
        vaultId: String,
        deviceId: String,
    ): CharArray? = synchronized(lock) {
        val current = store.read(HostedAccountClient.CHECKPOINT_KEY) ?: return null
        try {
            if (!current.contentEquals(checkpoint)) return null
        } finally {
            current.fill(0)
        }
        val secrets = VaultSecretStore(store)
        when (val pending = secrets.readPending(origin, accountId)) {
            is StoredSecret.Present -> pending.value
            StoredSecret.Missing -> when (val binding = secrets.read(vaultId, deviceId)) {
                is StoredSecret.Present -> binding.value
                StoredSecret.Missing -> null
            }
        }
    }

    /**
     * Commits only the exact checkpoint/candidate snapshot that was verified outside this lock.
     * A replacement checkpoint or pending value is retained untouched for a later retry.
     */
    fun commitVerified(
        store: HostedSecureStore,
        checkpoint: ByteArray,
        origin: String,
        accountId: String,
        vaultId: String,
        deviceId: String,
        candidate: CharArray,
    ): Boolean = synchronized(lock) {
        val current = store.read(HostedAccountClient.CHECKPOINT_KEY) ?: return false
        try {
            if (!current.contentEquals(checkpoint)) return false
        } finally {
            current.fill(0)
        }

        val secrets = VaultSecretStore(store)
        val pendingMatches = when (val pending = secrets.readPending(origin, accountId)) {
            is StoredSecret.Present -> try {
                pending.value.contentEquals(candidate)
            } finally {
                pending.value.fill('\u0000')
            }
            StoredSecret.Missing -> when (val binding = secrets.read(vaultId, deviceId)) {
                is StoredSecret.Present -> try {
                    binding.value.contentEquals(candidate)
                } finally {
                    binding.value.fill('\u0000')
                }
                StoredSecret.Missing -> false
            }
        }
        if (!pendingMatches || !secrets.saveAndReadbackIfMissing(vaultId, deviceId, candidate)) return false

        when (val pending = secrets.readPending(origin, accountId)) {
            is StoredSecret.Present -> try {
                if (!pending.value.contentEquals(candidate)) return false
                try {
                    store.remove(secrets.pendingAccount(origin, accountId))
                } catch (_: Exception) {
                    return false
                }
            } finally {
                pending.value.fill('\u0000')
            }
            StoredSecret.Missing -> Unit
        }
        store.remove(HostedAccountClient.CHECKPOINT_KEY)
        true
    }

    fun <T> withStorageLock(body: () -> T): T = synchronized(lock) { body() }

    @VisibleForTesting
    internal fun resetForTest() {
        storeFactory = ::AndroidHostedSecureStore
    }
}
