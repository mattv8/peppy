# Peppy libadb fork

This module is a modified fork of [libadb-android 3.1.1](https://github.com/MuntashirAkon/libadb-android).

## Hardening changes

The fork closes pairing socket publication and cancellation races; serializes and
makes SPAKE2 native-context destruction idempotent; creates a fresh Conscrypt TLS
context for each ephemeral key; rejects DNS hostnames; applies total pairing
deadlines; removes sensitive pairing logs; wipes password and exported TLS-key
buffers; and supports cancellation without holding the manager lock during
blocking pairing. It reports a definite SPAKE2 peer-info authentication failure
as `AdbPairingCodeRejectedException`, without changing SPAKE2, TLS-exporter, or
framing semantics.

## Third-party dependencies and licenses

- Conscrypt (`org.conscrypt:conscrypt-android:2.5.3`) — Apache-2.0.
- spake2-android (`com.github.MuntashirAkon.spake2-java:spake2-android:2.2.1`) — LGPL-3.0. Its dynamically loaded native library is a separate `libspake2.so`. Source: <https://github.com/MuntashirAkon/spake2-java/tree/v2.2.1>.
- Bouncy Castle (`org.bouncycastle:bcprov-jdk15to18:1.81`, `org.bouncycastle:bcpkix-jdk15to18:1.81`, and transitive `org.bouncycastle:bcutil-jdk15to18:1.81`) — MIT-style license.

The upstream Apache-2.0 license is in [LICENSE](LICENSE). Source files retain
their upstream license headers.
