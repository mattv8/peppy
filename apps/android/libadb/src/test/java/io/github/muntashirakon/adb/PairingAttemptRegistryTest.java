package io.github.muntashirakon.adb;

import static org.junit.Assert.assertSame;
import static org.junit.Assert.assertThrows;

import org.junit.Test;

import java.io.IOException;

public class PairingAttemptRegistryTest {
    @Test
    public void rejectedSecondAttemptDoesNotUnregisterActiveAttempt() throws IOException {
        PairingAttemptRegistry<Object> registry = new PairingAttemptRegistry<>();
        Object active = new Object();
        Object first = registry.begin();
        registry.activate(first, active);

        assertThrows(IllegalStateException.class, registry::begin);
        assertSame(active, registry.cancel());
    }

    @Test
    public void cancellationWithNothingPendingDoesNotPoisonNextAttempt() throws IOException {
        PairingAttemptRegistry<Object> registry = new PairingAttemptRegistry<>();
        registry.cancel();
        Object pending = registry.begin();
        Object attempt = new Object();
        registry.activate(pending, attempt);

        assertSame(attempt, registry.cancel());
    }

    @Test
    public void cancellationDuringPendingBuildRejectsThatAttempt() {
        PairingAttemptRegistry<Object> registry = new PairingAttemptRegistry<>();
        Object pending = registry.begin();
        registry.cancel();

        assertThrows(IOException.class, () -> registry.activate(pending, new Object()));
    }

    @Test
    public void cancellationAfterUnregisterIsNoOp() throws IOException {
        PairingAttemptRegistry<Object> registry = new PairingAttemptRegistry<>();
        Object first = new Object();
        Object firstPending = registry.begin();
        registry.activate(firstPending, first);
        registry.unregister(firstPending, first);
        registry.cancel();

        Object secondPending = registry.begin();
        Object second = new Object();
        registry.activate(secondPending, second);
        assertSame(second, registry.cancel());
    }
}
