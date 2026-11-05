// SPDX-License-Identifier: GPL-3.0-or-later OR Apache-2.0

package io.github.muntashirakon.adb;

import android.net.InetAddresses;
import android.os.Build;
import android.util.Log;

import androidx.annotation.NonNull;
import androidx.annotation.Nullable;
import androidx.annotation.RequiresApi;

import java.io.Closeable;
import java.io.DataInputStream;
import java.io.DataOutputStream;
import java.io.EOFException;
import java.io.IOException;
import java.net.InetAddress;
import java.net.InetSocketAddress;
import java.net.Socket;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.security.InvalidKeyException;
import java.security.KeyManagementException;
import java.security.NoSuchAlgorithmException;
import java.security.PrivateKey;
import java.security.cert.Certificate;
import java.security.interfaces.RSAPublicKey;
import java.util.Arrays;
import java.util.Objects;
import java.util.concurrent.atomic.AtomicBoolean;

import javax.net.ssl.SSLContext;
import javax.net.ssl.SSLException;
import javax.net.ssl.SSLServerSocket;
import javax.net.ssl.SSLSocket;

// https://github.com/aosp-mirror/platform_system_core/blob/android-11.0.0_r1/adb/pairing_connection/pairing_connection.cpp
// Also based on Shizuku's implementation
@RequiresApi(Build.VERSION_CODES.GINGERBREAD)
public final class PairingConnectionCtx implements Closeable, PairingAttempt {
    public static final String TAG = PairingConnectionCtx.class.getSimpleName();

    public static final String EXPORTED_KEY_LABEL = "adb-label\u0000";
    public static final int EXPORT_KEY_SIZE = 64;

    private enum State {
        Ready,
        ExchangingMsgs,
        ExchangingPeerInfo,
        Stopped
    }

    enum Role {
        Client,
        Server,
    }

    private static final int DEFAULT_TIMEOUT_MILLIS = 5_000;
    private final InetAddress mHost;
    private final int mPort;
    private final int mTimeoutMillis;
    private final byte[] mPswd;
    private final PeerInfo mPeerInfo;
    private final SSLContext mSslContext;
    private final Role mRole = Role.Client;
    private final SocketFactory mSocketFactory;

    private volatile DataInputStream mInputStream;
    private volatile DataOutputStream mOutputStream;
    private volatile PairingAuthCtx mPairingAuthCtx;
    private volatile State mState = State.Ready;
    private final Object mLifecycleLock = new Object();
    private final AtomicBoolean mClosed = new AtomicBoolean(false);
    private final PairingSocketOwner mSocket = new PairingSocketOwner();

    interface SocketFactory {
        Socket create() throws IOException;
    }

    public PairingConnectionCtx(@NonNull String host, int port, @NonNull byte[] pswd, @NonNull KeyPair keyPair,
                                 @NonNull String deviceName)
            throws NoSuchAlgorithmException, KeyManagementException, InvalidKeyException {
        this(host, port, pswd, keyPair, deviceName, DEFAULT_TIMEOUT_MILLIS);
    }

    public PairingConnectionCtx(@NonNull String host, int port, @NonNull byte[] pswd, @NonNull KeyPair keyPair,
                                 @NonNull String deviceName, int timeoutMillis)
            throws NoSuchAlgorithmException, KeyManagementException, InvalidKeyException {
        this(parseNumericHost(host), port, pswd, keyPair, deviceName, timeoutMillis, Socket::new);
    }

    PairingConnectionCtx(@NonNull String host, int port, @NonNull byte[] pswd, @NonNull KeyPair keyPair,
                          @NonNull String deviceName, int timeoutMillis, @NonNull SocketFactory socketFactory)
            throws NoSuchAlgorithmException, KeyManagementException, InvalidKeyException {
        this(parseNumericHost(host), port, pswd, keyPair, deviceName, timeoutMillis, socketFactory);
    }

    PairingConnectionCtx(@NonNull InetAddress host, int port, @NonNull byte[] pswd, @NonNull KeyPair keyPair,
                         @NonNull String deviceName, int timeoutMillis, @NonNull SocketFactory socketFactory)
            throws NoSuchAlgorithmException, KeyManagementException, InvalidKeyException {
        this(host, port, pswd, keyPair, deviceName, timeoutMillis, socketFactory, SslUtils.getSslContext(keyPair));
    }

    PairingConnectionCtx(@NonNull InetAddress host, int port, @NonNull byte[] pswd, @NonNull KeyPair keyPair,
                         @NonNull String deviceName, int timeoutMillis, @NonNull SocketFactory socketFactory,
                         @Nullable SSLContext sslContext) throws InvalidKeyException {
        this.mHost = Objects.requireNonNull(host);
        if (port < 1 || port > 65535) throw new IllegalArgumentException("Invalid port");
        if (timeoutMillis <= 0) throw new IllegalArgumentException("Timeout must be positive");
        this.mPort = port;
        this.mTimeoutMillis = timeoutMillis;
        this.mSocketFactory = Objects.requireNonNull(socketFactory);
        this.mPswd = Objects.requireNonNull(pswd);
        this.mPeerInfo = new PeerInfo(PeerInfo.ADB_RSA_PUB_KEY, AndroidPubkey.encodeWithName((RSAPublicKey)
                keyPair.getPublicKey(), Objects.requireNonNull(deviceName)));
        this.mSslContext = sslContext;
    }

    public PairingConnectionCtx(@NonNull String host, int port, @NonNull byte[] pswd, @NonNull PrivateKey privateKey,
                                @NonNull Certificate certificate, @NonNull String deviceName)
            throws NoSuchAlgorithmException, KeyManagementException, InvalidKeyException {
        this(host, port, pswd, new KeyPair(Objects.requireNonNull(privateKey), Objects.requireNonNull(certificate)),
                deviceName);
    }

    public PairingConnectionCtx(@NonNull String host, int port, @NonNull byte[] pswd, @NonNull PrivateKey privateKey,
                                @NonNull Certificate certificate, @NonNull String deviceName, int timeoutMillis)
            throws NoSuchAlgorithmException, KeyManagementException, InvalidKeyException {
        this(host, port, pswd, new KeyPair(Objects.requireNonNull(privateKey), Objects.requireNonNull(certificate)),
                deviceName, timeoutMillis);
    }

    public void start() throws IOException {
        if (mState != State.Ready) {
            throw new IOException("Connection is not ready yet.");
        }

        mState = State.ExchangingMsgs;
        PairingDeadline deadline = new PairingDeadline(mTimeoutMillis, this::closeOwnedSocket);
        deadline.start();
        boolean completed = false;
        try {
            setupTlsConnection(deadline);
            for (; ; ) {
            switch (mState) {
                case ExchangingMsgs:
                    if (!doExchangeMsgs()) {
                        notifyResult();
                        throw new IOException("Exchanging message wasn't successful.");
                    }
                    mState = State.ExchangingPeerInfo;
                    break;
                case ExchangingPeerInfo:
                    doExchangePeerInfo();
                    notifyResult();
                    completed = true;
                    return;
                case Ready:
                case Stopped:
                    throw new IOException("Connection closed with errors.");
            }
            }
        } finally {
            deadline.close();
            if (!completed) closeOwnedSocket();
        }
    }

    private void notifyResult() {
        mState = State.Stopped;
    }

    private void setupTlsConnection(PairingDeadline deadline) throws IOException {
        Socket socket;
        if (mRole == Role.Server) {
            SSLServerSocket sslServerSocket = (SSLServerSocket) mSslContext.getServerSocketFactory().createServerSocket(mPort);
            socket = sslServerSocket.accept();
            publishSocket(socket);
            // TODO: Write automated test scripts after removing Conscrypt dependency.
        } else { // role == Role.Client
            socket = mSocketFactory.create();
            publishSocket(socket);
            socket.connect(new InetSocketAddress(mHost, mPort), deadline.remainingMillis());
        }
        deadline.check();
        socket.setTcpNoDelay(true);
        socket.setSoTimeout(deadline.remainingMillis());

        // We use custom SSLContext to allow any SSL certificates
        SSLSocket sslSocket = (SSLSocket) mSslContext.getSocketFactory().createSocket(socket,
                mHost.getHostAddress(), mPort, true);
        handoffSocket(socket, sslSocket);
        sslSocket.setSoTimeout(deadline.remainingMillis());
        sslSocket.startHandshake();
        deadline.check();

        mInputStream = new DataInputStream(sslSocket.getInputStream());
        mOutputStream = new DataOutputStream(sslSocket.getOutputStream());

        // To ensure the connection is not stolen while we do the PAKE, append the exported key material from the
        // tls connection to the password.
        byte[] keyMaterial = exportKeyingMaterial(sslSocket, EXPORT_KEY_SIZE);
        byte[] passwordBytes = new byte[mPswd.length + keyMaterial.length];
        try {
            System.arraycopy(mPswd, 0, passwordBytes, 0, mPswd.length);
            System.arraycopy(keyMaterial, 0, passwordBytes, mPswd.length, keyMaterial.length);
            PairingAuthCtx pairingAuthCtx = PairingAuthCtx.createAlice(passwordBytes);
            if (pairingAuthCtx == null) throw new IOException("Unable to create PairingAuthCtx.");
            synchronized (mLifecycleLock) {
                if (mClosed.get()) {
                    pairingAuthCtx.destroy();
                    throw new IOException("Connection closed.");
                }
                this.mPairingAuthCtx = pairingAuthCtx;
            }
        } finally {
            Arrays.fill(passwordBytes, (byte) 0);
            Arrays.fill(keyMaterial, (byte) 0);
        }
    }

    private byte[] exportKeyingMaterial(SSLSocket sslSocket, int length) throws SSLException {
        if (!org.conscrypt.Conscrypt.isConscrypt(sslSocket))
            throw new SSLException("Pairing requires the bundled Conscrypt socket.");
        return org.conscrypt.Conscrypt.exportKeyingMaterial(sslSocket, EXPORTED_KEY_LABEL, null, length);
    }

    private void writeHeader(@NonNull PairingPacketHeader header, @NonNull byte[] payload) throws IOException {
        ByteBuffer buffer = ByteBuffer.allocate(PairingPacketHeader.PAIRING_PACKET_HEADER_SIZE)
                .order(ByteOrder.BIG_ENDIAN);
        header.writeTo(buffer);

        mOutputStream.write(buffer.array());
        mOutputStream.write(payload);
    }

    @Nullable
    private PairingPacketHeader readHeader() throws IOException {
        byte[] bytes = new byte[PairingPacketHeader.PAIRING_PACKET_HEADER_SIZE];
        mInputStream.readFully(bytes);
        ByteBuffer buffer = ByteBuffer.wrap(bytes).order(ByteOrder.BIG_ENDIAN);
        return PairingPacketHeader.readFrom(buffer);
    }

    @NonNull
    private PairingPacketHeader createHeader(byte type, int payloadSize) {
        return new PairingPacketHeader(PairingPacketHeader.CURRENT_KEY_HEADER_VERSION, type, payloadSize);
    }

    private boolean checkHeaderType(byte expected, byte actual) {
        if (expected != actual) {
            Log.e(TAG, "Unexpected header type (expected=" + expected + " actual=" + actual + ")");
            return false;
        }
        return true;
    }

    private boolean doExchangeMsgs() throws IOException {
        byte[] msg = mPairingAuthCtx.getMsg();

        PairingPacketHeader ourHeader = createHeader(PairingPacketHeader.SPAKE2_MSG, msg.length);
        // Write our SPAKE2 msg
        writeHeader(ourHeader, msg);

        // Read the peer's SPAKE2 msg header
        PairingPacketHeader theirHeader = readHeader();
        if (theirHeader == null || !checkHeaderType(PairingPacketHeader.SPAKE2_MSG, theirHeader.type)) return false;

        // Read the SPAKE2 msg payload and initialize the cipher for encrypting the PeerInfo and certificate.
        byte[] theirMsg = new byte[theirHeader.payloadSize];
        mInputStream.readFully(theirMsg);

        try {
            return mPairingAuthCtx.initCipher(theirMsg);
        } catch (Exception e) {
            Log.e(TAG, "Unable to initialize pairing cipher");
            //noinspection UnnecessaryInitCause
            throw (IOException) new IOException().initCause(e);
        }
    }

    private void doExchangePeerInfo() throws IOException {
        // Encrypt PeerInfo
        ByteBuffer buffer = ByteBuffer.allocate(PeerInfo.MAX_PEER_INFO_SIZE).order(ByteOrder.BIG_ENDIAN);
        mPeerInfo.writeTo(buffer);
        byte[] outBuffer = mPairingAuthCtx.encrypt(buffer.array());
        if (outBuffer == null) {
            Log.e(TAG, "Failed to encrypt peer info");
            throw new IOException("Could not encrypt peer info.");
        }

        // Write out the packet header
        PairingPacketHeader ourHeader = createHeader(PairingPacketHeader.PEER_INFO, outBuffer.length);
        // Write out the encrypted payload
        writeHeader(ourHeader, outBuffer);

        // Read in the peer's packet header
        PairingPacketHeader theirHeader;
        try {
            theirHeader = readHeader();
        } catch (EOFException exception) {
            throw peerClosedAfterSpake(exception);
        }
        if (theirHeader == null || !checkHeaderType(PairingPacketHeader.PEER_INFO, theirHeader.type)) {
            throw new IOException("Unexpected peer info header.");
        }

        // Read in the encrypted peer certificate
        byte[] theirMsg = new byte[theirHeader.payloadSize];
        try {
            mInputStream.readFully(theirMsg);
        } catch (EOFException exception) {
            throw peerClosedAfterSpake(exception);
        }

        // Try to decrypt the certificate
        byte[] decryptedMsg = mPairingAuthCtx.decrypt(theirMsg);
        if (decryptedMsg == null) {
            Log.e(TAG, "Unsupported payload while decrypting peer info.");
            throw peerInfoDecryptionFailed();
        }

        // The decrypted message should contain the PeerInfo.
        if (decryptedMsg.length != PeerInfo.MAX_PEER_INFO_SIZE) {
            Log.e(TAG, "Got size=" + decryptedMsg.length + " PeerInfo.size=" + PeerInfo.MAX_PEER_INFO_SIZE);
            throw new IOException("Invalid peer info payload.");
        }

        PeerInfo.readFrom(ByteBuffer.wrap(decryptedMsg));
    }

    private IOException peerClosedAfterSpake(IOException exception) {
        return peerInfoReadFailure(exception, mClosed.get());
    }

    static AdbPairingCodeRejectedException peerInfoDecryptionFailed() {
        return new AdbPairingCodeRejectedException();
    }

    static IOException peerInfoReadFailure(IOException exception, boolean locallyClosed) {
        if (locallyClosed || !(exception instanceof EOFException)) return exception;
        return new AdbPairingCodeRejectedException();
    }

    @Override
    public void close() {
        mClosed.set(true);
        mState = State.Stopped;
        Arrays.fill(mPswd, (byte) 0);
        closeOwnedSocket();
        if (mInputStream != null) {
            try {
                mInputStream.close();
            } catch (IOException ignore) {
            }
        }
        if (mOutputStream != null) {
            try {
                mOutputStream.close();
            } catch (IOException ignore) {
            }
        }
        synchronized (mLifecycleLock) {
            PairingAuthCtx pairingAuthCtx = mPairingAuthCtx;
            if (pairingAuthCtx != null) pairingAuthCtx.destroy();
        }
    }

    private void publishSocket(Socket socket) throws IOException {
        mSocket.publish(socket, mClosed);
    }

    private void handoffSocket(Socket socket, SSLSocket sslSocket) throws IOException {
        mSocket.handoff(socket, sslSocket, mClosed);
    }

    private void closeOwnedSocket() {
        mSocket.close();
    }

    static InetAddress parseNumericHost(String host) {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.Q)
            throw new IllegalArgumentException("Numeric hosts require API 29 or newer.");
        try {
            return InetAddresses.parseNumericAddress(Objects.requireNonNull(host));
        } catch (IllegalArgumentException e) {
            throw new IllegalArgumentException("A numeric host is required.", e);
        }
    }


    private static class PeerInfo {
        public static final int MAX_PEER_INFO_SIZE = 1 << 13;

        public static final byte ADB_RSA_PUB_KEY = 0;
        public static final byte ADB_DEVICE_GUID = 0;

        @NonNull
        public static PeerInfo readFrom(@NonNull ByteBuffer buffer) {
            byte type = buffer.get();
            byte[] data = new byte[MAX_PEER_INFO_SIZE - 1];
            buffer.get(data);
            return new PeerInfo(type, data);
        }

        private final byte type;
        private final byte[] data = new byte[MAX_PEER_INFO_SIZE - 1];

        public PeerInfo(byte type, byte[] data) {
            this.type = type;
            System.arraycopy(data, 0, this.data, 0, Math.min(data.length, MAX_PEER_INFO_SIZE - 1));
        }

        public void writeTo(@NonNull ByteBuffer buffer) {
            buffer.put(type).put(data);
        }

    }

    private static class PairingPacketHeader {
        public static final byte CURRENT_KEY_HEADER_VERSION = 1;
        public static final byte MIN_SUPPORTED_KEY_HEADER_VERSION = 1;
        public static final byte MAX_SUPPORTED_KEY_HEADER_VERSION = 1;

        public static final int MAX_PAYLOAD_SIZE = 2 * PeerInfo.MAX_PEER_INFO_SIZE;
        public static final byte PAIRING_PACKET_HEADER_SIZE = 6;

        public static final byte SPAKE2_MSG = 0;
        public static final byte PEER_INFO = 1;

        @Nullable
        public static PairingPacketHeader readFrom(@NonNull ByteBuffer buffer) {
            byte version = buffer.get();
            byte type = buffer.get();
            int payload = buffer.getInt();
            if (version < MIN_SUPPORTED_KEY_HEADER_VERSION || version > MAX_SUPPORTED_KEY_HEADER_VERSION) {
                Log.e(TAG, "PairingPacketHeader version mismatch (us=" + CURRENT_KEY_HEADER_VERSION
                        + " them=" + version + ")");
                return null;
            }
            if (type != SPAKE2_MSG && type != PEER_INFO) {
                Log.e(TAG, "Unknown PairingPacket type " + type);
                return null;
            }
            if (payload <= 0 || payload > MAX_PAYLOAD_SIZE) {
                Log.e(TAG, "Header payload not within a safe payload size (size=" + payload + ")");
                return null;
            }
            return new PairingPacketHeader(version, type, payload);
        }

        private final byte version;
        private final byte type;
        private final int payloadSize;

        public PairingPacketHeader(byte version, byte type, int payloadSize) {
            this.version = version;
            this.type = type;
            this.payloadSize = payloadSize;
        }

        public void writeTo(@NonNull ByteBuffer buffer) {
            buffer.put(version).put(type).putInt(payloadSize);
        }

        @NonNull
        @Override
        public String toString() {
            return "PairingPacketHeader{" +
                    "version=" + version +
                    ", type=" + type +
                    ", payloadSize=" + payloadSize +
                    '}';
        }
    }
}
