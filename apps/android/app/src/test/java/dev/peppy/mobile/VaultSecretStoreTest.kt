package dev.peppy.mobile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.IOException

class VaultSecretStoreTest {
    private class MemorySecureStore : HostedSecureStore {
        val values = mutableMapOf<String, ByteArray>()
        var failRead = false
        var failWrite = false
        var failWriteKeyPrefix: String? = null

        override fun read(key: String): ByteArray? {
            if (failRead) throw IOException("unavailable")
            return values[key]?.clone()
        }

        override fun write(key: String, value: ByteArray) {
            if (failWrite || failWriteKeyPrefix?.let { key.startsWith(it) } == true) throw IOException("unavailable")
            values[key] = value.clone()
        }

        override fun remove(key: String) {
            values.remove(key)
        }
    }

    @Test fun `stores secrets under the versioned binding account`() {
        val store = VaultSecretStore(MemorySecureStore())
        assertEquals("vault-secret:v2:vault:device", store.account("vault", "device"))
    }

    @Test fun `saves only a missing secret and reads a defensive character copy`() {
        val secure = MemorySecureStore()
        val store = VaultSecretStore(secure)
        assertTrue(store.saveIfMissing("vault", "device", "secret".toCharArray()))
        assertFalse(store.saveIfMissing("vault", "device", "other".toCharArray()))
        val value = (store.read("vault", "device") as StoredSecret.Present).value
        assertEquals("secret", String(value))
        value.fill('\u0000')
    }

    @Test fun `turns secure store read failures into unavailable`() {
        val secure = MemorySecureStore().apply { failRead = true }
        try {
            VaultSecretStore(secure).read("vault", "device")
            throw AssertionError("expected secure store failure")
        } catch (_: SecureStoreUnavailable) {
        }
    }

    @Test fun movingPendingToBindingTombstonesThePendingSlot() {
        val store = VaultSecretStore(MemorySecureStore())
        val origin = "https://enroll.example"
        assertTrue(store.savePending(origin, "pending".toCharArray()))
        store.movePendingToBinding(origin, "vault", "device")
        val binding = (store.read("vault", "device") as StoredSecret.Present).value
        assertEquals("pending", String(binding))
        binding.fill('\u0000')
        assertEquals(StoredSecret.Missing, store.readPending(origin))
    }

    @Test fun pendingSecretIsWrittenOnceAndNeverReplaced() {
        val store = VaultSecretStore(MemorySecureStore())
        assertTrue(store.savePending("https://enroll.example", "first".toCharArray()))
        assertTrue(store.savePending("https://enroll.example", "second".toCharArray()))
        val value = (store.readPending("https://enroll.example") as StoredSecret.Present).value
        assertEquals("first", String(value))
        value.fill('\u0000')
    }

    @Test fun failingPendingWriteReturnsFalseSoCreationCanAbort() {
        val secure = MemorySecureStore().apply { failWrite = true }
        assertFalse(VaultSecretStore(secure).savePending("https://enroll.example", "secret".toCharArray()))
        assertEquals(StoredSecret.Missing, VaultSecretStore(secure).readPending("https://enroll.example"))
    }

    @Test fun pendingReadFailureIsUnavailable() {
        val secure = MemorySecureStore().apply { failRead = true }
        try {
            VaultSecretStore(secure).readPending("https://enroll.example")
            throw AssertionError("expected secure store failure")
        } catch (_: SecureStoreUnavailable) {
        }
    }

    @Test fun pendingMovesToMissingBindingAndTombstonesThePendingSlot() {
        val secure = MemorySecureStore()
        val store = VaultSecretStore(secure)
        store.savePending("https://enroll.example", "secret".toCharArray())
        store.movePendingToBinding("https://enroll.example", "vault", "device")
        assertEquals("secret", String((store.read("vault", "device") as StoredSecret.Present).value))
        assertEquals(StoredSecret.Missing, store.readPending("https://enroll.example"))
    }

    @Test fun failedBindingWriteKeepsPendingWithoutThrowing() {
        val secure = MemorySecureStore().apply { failWriteKeyPrefix = "vault-secret:v2:" }
        val store = VaultSecretStore(secure)
        store.savePending("https://enroll.example", "secret".toCharArray())
        store.movePendingToBinding("https://enroll.example", "vault", "device")
        assertEquals(StoredSecret.Missing, store.read("vault", "device"))
        assertEquals("secret", String((store.readPending("https://enroll.example") as StoredSecret.Present).value))
    }

    @Test fun existingBindingIsNeverOverwrittenByPendingMove() {
        val secure = MemorySecureStore()
        val store = VaultSecretStore(secure)
        store.saveIfMissing("vault", "device", "bound".toCharArray())
        store.savePending("https://enroll.example", "pending".toCharArray())
        store.movePendingToBinding("https://enroll.example", "vault", "device")
        assertEquals("bound", String((store.read("vault", "device") as StoredSecret.Present).value))
        assertEquals("pending", String((store.readPending("https://enroll.example") as StoredSecret.Present).value))
    }

    @Test fun matchingExistingBindingTombstonesThePendingSlot() {
        val secure = MemorySecureStore()
        val store = VaultSecretStore(secure)
        store.saveIfMissing("vault", "device", "secret".toCharArray())
        store.savePending("https://enroll.example", "secret".toCharArray())

        store.movePendingToBinding("https://enroll.example", "vault", "device")

        assertEquals(StoredSecret.Missing, store.readPending("https://enroll.example"))
    }

    private val origin = "https://peppy.pro"

    @Test fun accountPendingSecretAcceptsOnlyMissingOrMatchingValues() {
        val store = VaultSecretStore(MemorySecureStore())
        assertTrue(store.savePending(origin, "account-a", "first".toCharArray()))
        assertTrue(store.savePending(origin, "account-a", "first".toCharArray()))
        assertFalse(store.savePending(origin, "account-a", "second".toCharArray()))
        assertEquals("first", String((store.readPending(origin, "account-a") as StoredSecret.Present).value))
        assertEquals(StoredSecret.Missing, store.readPending(origin, "account-b"))
        assertEquals(StoredSecret.Missing, store.readPending(origin))
    }

    @Test fun unreadableAccountPendingSecretIsNeverReplaced() {
        val secure = MemorySecureStore()
        val store = VaultSecretStore(secure)
        store.savePending(origin, "account-a", "first".toCharArray())
        secure.failRead = true
        assertFalse(store.savePending(origin, "account-a", "second".toCharArray()))
        assertFalse(store.movePendingToBinding(origin, "account-a", "vault", "device"))
        secure.failRead = false
        assertEquals("first", String((store.readPending(origin, "account-a") as StoredSecret.Present).value))
        assertEquals(StoredSecret.Missing, store.read("vault", "device"))
    }

    @Test fun accountPendingMoveVerifiesTheBindingBeforeRemovingPending() {
        val store = VaultSecretStore(MemorySecureStore())
        store.savePending(origin, "account-a", "secret".toCharArray())
        assertTrue(store.movePendingToBinding(origin, "account-a", "vault", "device"))
        assertEquals("secret", String((store.read("vault", "device") as StoredSecret.Present).value))
        assertEquals(StoredSecret.Missing, store.readPending(origin, "account-a"))
        assertTrue(store.movePendingToBinding(origin, "account-a", "vault", "device"))
        assertFalse(store.movePendingToBinding(origin, "account-a", "other-vault", "device"))
    }

    @Test fun accountPendingMoveNeverOverwritesADifferentBinding() {
        val store = VaultSecretStore(MemorySecureStore())
        store.saveIfMissing("vault", "device", "bound".toCharArray())
        store.savePending(origin, "account-a", "secret".toCharArray())
        assertFalse(store.movePendingToBinding(origin, "account-a", "vault", "device"))
        assertEquals("bound", String((store.read("vault", "device") as StoredSecret.Present).value))
        assertEquals("secret", String((store.readPending(origin, "account-a") as StoredSecret.Present).value))
    }

    @Test fun accountPendingMoveAdoptsOnlyItsOwnAccount() {
        val store = VaultSecretStore(MemorySecureStore())
        store.savePending(origin, "account-a", "secret".toCharArray())
        assertFalse(store.movePendingToBinding(origin, "account-b", "vault", "device"))
        assertEquals(StoredSecret.Missing, store.read("vault", "device"))
    }

    @Test fun failedAccountBindingWriteKeepsPending() {
        val secure = MemorySecureStore().apply { failWriteKeyPrefix = "vault-secret:v2:" }
        val store = VaultSecretStore(secure)
        store.savePending(origin, "account-a", "secret".toCharArray())
        assertFalse(store.movePendingToBinding(origin, "account-a", "vault", "device"))
        assertEquals("secret", String((store.readPending(origin, "account-a") as StoredSecret.Present).value))
    }
}
