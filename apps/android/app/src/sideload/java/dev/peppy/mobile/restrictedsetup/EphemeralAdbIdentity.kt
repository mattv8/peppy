package dev.peppy.mobile.restrictedsetup

import java.math.BigInteger
import java.security.KeyPairGenerator
import java.security.PrivateKey
import java.security.SecureRandom
import java.security.cert.X509Certificate
import java.util.Date
import org.bouncycastle.asn1.x500.X500Name
import org.bouncycastle.cert.jcajce.JcaX509v3CertificateBuilder
import org.bouncycastle.cert.jcajce.JcaX509CertificateConverter
import org.bouncycastle.operator.jcajce.JcaContentSignerBuilder

/** An ADB identity that exists only for one setup attempt. */
internal class EphemeralAdbIdentity private constructor(
    private var privateKeyValue: PrivateKey?,
    private var certificateValue: X509Certificate?,
) : AutoCloseable {
    val privateKey: PrivateKey
        get() = checkNotNull(privateKeyValue) { "Identity is closed" }
    val certificate: X509Certificate
        get() = checkNotNull(certificateValue) { "Identity is closed" }

    override fun close() = destroy()

    fun destroy() {
        privateKeyValue = null
        certificateValue = null
    }

    internal companion object {
        const val DEVICE_NAME = "Peppy"

        fun create(): EphemeralAdbIdentity {
            val keyPair = KeyPairGenerator.getInstance("RSA").apply { initialize(2048) }.generateKeyPair()
            val now = Date()
            val subject = X500Name("CN=$DEVICE_NAME")
            val certificate = JcaX509CertificateConverter().getCertificate(
                JcaX509v3CertificateBuilder(
                    subject,
                    BigInteger(160, SecureRandom()),
                    now,
                    Date(now.time + CERTIFICATE_LIFETIME_MILLIS),
                    subject,
                    keyPair.public,
                ).build(JcaContentSignerBuilder("SHA256withRSA").build(keyPair.private)),
            )
            return EphemeralAdbIdentity(keyPair.private, certificate)
        }

        private const val CERTIFICATE_LIFETIME_MILLIS = 24 * 60 * 60 * 1000L
    }
}
