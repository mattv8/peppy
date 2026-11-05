// SPDX-License-Identifier: BSD-3-Clause AND (GPL-3.0-or-later OR Apache-2.0)

package io.github.muntashirakon.adb;

import java.io.Closeable;
import java.io.IOException;
import java.nio.ByteBuffer;
import java.util.ArrayDeque;
import java.util.Queue;

/**
 * This class abstracts the underlying ADB streams
 */
// Copyright 2013 Cameron Gutman
public class AdbStream implements Closeable {

    /**
     * The AdbConnection object that the stream communicates over
     */
    private final AdbConnection mAdbConnection;

    /**
     * The local ID of the stream
     */
    private final int mLocalId;

    /**
     * The remote ID of the stream
     */
    private volatile int mRemoteId;

    /**
     * Indicates whether WRTE is currently allowed
     */
    private boolean mWriteReady;

    /**
     * A queue of data from the target's WRTE packets
     */
    private final Queue<byte[]> mReadQueue;

    /**
     * Store data received from the first WRTE packet in order to support buffering.
     */
    private final ByteBuffer mReadBuffer;

    /**
     * Indicates whether the connection is closed already
     */
    private boolean mIsClosed;

    /**
     * Whether the remote peer has closed but we still have unread data in the queue
     */
    private boolean mPeerClosed;

    private boolean mOpenAcknowledged;

    private IOException mFailure;

    /**
     * Creates a new AdbStream object on the specified AdbConnection
     * with the given local ID.
     *
     * @param adbConnection AdbConnection that this stream is running on
     * @param localId       Local ID of the stream
     */
    AdbStream(AdbConnection adbConnection, int localId)
            throws IOException, InterruptedException, AdbPairingRequiredException {
        this.mAdbConnection = adbConnection;
        this.mLocalId = localId;
        this.mReadQueue = new ArrayDeque<>();
        this.mReadBuffer = (ByteBuffer) ByteBuffer.allocate(adbConnection.getMaxData()).flip();
        this.mWriteReady = false;
        this.mIsClosed = false;
    }

    public AdbInputStream openInputStream() {
        return new AdbInputStream(this);
    }

    public AdbOutputStream openOutputStream() {
        return new AdbOutputStream(this);
    }

    /**
     * Called by the connection thread to indicate newly received data.
     *
     * @param payload Data inside the WRTE message
     */
    synchronized void addPayload(byte[] payload) {
        if (!mIsClosed) {
            mReadQueue.add(payload);
            notifyAll();
        }
    }

    /**
     * Called by the connection thread to send an OKAY packet, allowing the
     * other side to continue transmission.
     *
     * @throws IOException If the connection fails while sending the packet
     */
    void sendReady() throws IOException {
        // Generate and send a OKAY packet
        mAdbConnection.sendPacket(AdbProtocol.generateReady(mLocalId, mRemoteId));
    }

    /**
     * Called by the connection thread to update the remote ID for this stream
     *
     * @param remoteId New remote ID
     */
    synchronized boolean acknowledgeOpen(int remoteId) {
        if (mIsClosed || mPeerClosed || mFailure != null) {
            return false;
        }
        this.mRemoteId = remoteId;
        mOpenAcknowledged = true;
        mWriteReady = true;
        notifyAll();
        return true;
    }

    /**
     * Called by the connection thread to notify that the stream was closed by the peer.
     */
    synchronized void notifyClose(boolean closedByPeer) {
        // We don't call close() because it sends another CLSE
        if (closedByPeer) {
            if (mFailure == null && !mIsClosed) {
                mPeerClosed = true;
            }
        } else {
            if (mFailure == null && !mPeerClosed) {
                mFailure = new IOException("Stream closed.");
                mIsClosed = true;
            }
        }
        notifyAll();
    }

    synchronized void notifyFailure(IOException failure) {
        if (!mPeerClosed && mFailure == null) {
            mFailure = failure;
            mIsClosed = true;
            notifyAll();
        }
    }

    /**
     * Read bytes from the ADB daemon.
     *
     * @return the next byte of data, or {@code -1} if the end of the stream is reached.
     * @throws IOException If the stream fails while waiting
     */
    public synchronized int read(byte[] bytes, int offset, int length) throws IOException {
        if (offset < 0 || length < 0 || offset > bytes.length - length) {
            throw new IndexOutOfBoundsException();
        }
        if (length == 0) return 0;
        if (mReadBuffer.hasRemaining()) {
            return readBuffer(bytes, offset, length);
        }
        byte[] data;
        while ((data = mReadQueue.poll()) == null) {
            if (mFailure != null) throw mFailure;
            if (mPeerClosed) return -1;
            try {
                wait();
            } catch (InterruptedException e) {
                Thread.currentThread().interrupt();
                IOException failure = new IOException(e);
                notifyFailure(failure);
                if (mFailure == failure) {
                    throw failure;
                }
            }
        }
        mReadBuffer.clear();
        mReadBuffer.put(data);
        mReadBuffer.flip();
        return mReadBuffer.hasRemaining() ? readBuffer(bytes, offset, length) : read(bytes, offset, length);
    }

    private int readBuffer(byte[] bytes, int offset, int length) {
        int count = 0;
        for (int i = offset; i < offset + length; ++i) {
            if (mReadBuffer.hasRemaining()) {
                bytes[i] = mReadBuffer.get();
                ++count;
            }
        }
        return count;
    }

    /**
     * Sends a WRTE packet with a given byte array payload. It does not flush the stream.
     *
     * @param bytes Payload in the form of a byte array
     * @throws IOException If the stream fails while sending data
     */
    public void write(byte[] bytes, int offset, int length) throws IOException {
        synchronized (this) {
            // Make sure we're ready for a WRTE
            while (!mIsClosed && !mPeerClosed && !mWriteReady) {
                try {
                    wait();
                } catch (InterruptedException e) {
                    Thread.currentThread().interrupt();
                    IOException failure = new IOException(e);
                    notifyFailure(failure);
                    if (mFailure == failure) {
                        throw failure;
                    }
                }
            }

            if (mIsClosed || mPeerClosed) {
                if (mFailure != null) throw mFailure;
                throw new IOException("Stream closed");
            }
            mWriteReady = false;
        }
        // Split and send data as WRTE packet
        // TODO: A WRITE message may not be sent until a READY message is received.
        //  Once a WRITE message is sent, an additional WRITE message may not be
        //  sent until another READY message has been received.  Recipients of
        //  a WRITE message that is in violation of this requirement will CLOSE
        //  the connection.
        int maxData;
        try {
            maxData = mAdbConnection.getMaxData();
        } catch (InterruptedException | AdbPairingRequiredException e) {
            //noinspection UnnecessaryInitCause
            throw (IOException) new IOException().initCause(e);
        }
        while (length != 0) {
            if (length <= maxData) {
                mAdbConnection.sendPacket(AdbProtocol.generateWrite(mLocalId, mRemoteId, bytes, offset, length));
                offset = offset + length;
                length = 0;
            } else { // if (length > maxData) {
                mAdbConnection.sendPacket(AdbProtocol.generateWrite(mLocalId, mRemoteId, bytes, offset, maxData));
                offset = offset + maxData;
                length = length - maxData;
            }
        }
    }

    public void flush() throws IOException {
        synchronized (this) {
            if (mFailure != null) throw mFailure;
            if (mIsClosed || mPeerClosed) {
                throw new IOException("Stream closed");
            }
        }
        mAdbConnection.flushPacket();
    }

    /**
     * Closes the stream. This sends a close message to the peer.
     *
     * @throws IOException If the stream fails while sending the close message.
     */
    @Override
    public void close() throws IOException {
        synchronized (this) {
            if (mIsClosed || mPeerClosed) return;
            notifyClose(false);
        }

        mAdbConnection.sendPacket(AdbProtocol.generateClose(mLocalId, mRemoteId));
    }

    /**
     * Returns whether the stream is closed or not
     *
     * @return True if the stream is close, false if not
     */
    public synchronized boolean isClosed() {
        return mIsClosed || mPeerClosed;
    }

    synchronized boolean isOpenAcknowledged() {
        return mOpenAcknowledged;
    }

    synchronized IOException getFailure() {
        return mFailure;
    }

    synchronized int getRemoteId() {
        return mRemoteId;
    }

    /**
     * Returns an estimate of available data.
     *
     * @return an estimate of the number of bytes that can be read from this stream without blocking.
     * @throws IOException if the stream is close.
     */
    public synchronized int available() throws IOException {
            if (mFailure != null) throw mFailure;
            if (mReadBuffer.hasRemaining()) {
                return mReadBuffer.remaining();
            }
            byte[] data = mReadQueue.peek();
            return data == null ? 0 : data.length;
    }
}
