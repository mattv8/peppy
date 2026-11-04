package io.github.muntashirakon.adb;

import static org.junit.Assert.assertThrows;
import static org.junit.Assert.assertTrue;

import org.junit.Test;

import java.io.IOException;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.TimeUnit;

public class PairingDeadlineTest {
    @Test
    public void expiresAcrossAllOperationsAndRunsCloseCallback() throws Exception {
        CountDownLatch expired = new CountDownLatch(1);
        PairingDeadline deadline = new PairingDeadline(20, expired::countDown);
        deadline.start();

        assertTrue(expired.await(1, TimeUnit.SECONDS));
        assertThrows(IOException.class, deadline::check);
    }
}
