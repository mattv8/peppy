import java.io.ByteArrayInputStream
import java.security.cert.CertificateFactory
import java.security.cert.X509Certificate
import java.util.Base64
import javax.xml.parsers.DocumentBuilderFactory

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("org.jetbrains.kotlin.plugin.compose")
}

fun debugEnabled(): Boolean {
    val value = System.getenv("DEBUG") ?: ""
    return when (value) {
        "" -> false
        "true", "1" -> true
        "false", "0" -> false
        else -> throw GradleException("DEBUG must be true, false, 1, or 0; got '$value'")
    }
}

val selectedAndroidAbi = providers.gradleProperty("peppyAndroidAbi").orNull?.let { value ->
    value.takeIf { it in setOf("arm64-v8a", "x86_64") }
        ?: throw GradleException("peppyAndroidAbi must be arm64-v8a or x86_64; got '$value'")
}

fun strictDebugBoolean(name: String): Boolean = providers.gradleProperty(name).orNull?.let { value ->
    when (value) {
        "true" -> true
        "false" -> false
        else -> throw GradleException("$name must be true or false; got '$value'")
    }
} ?: false

val peppySmsDiagnostics = strictDebugBoolean("peppySmsDiagnostics")

private val certificatePemPattern = Regex(
    """\A\s*-----BEGIN CERTIFICATE-----\s*([A-Za-z0-9+/=\s]+)-----END CERTIFICATE-----\s*\z""",
)

fun validatedDevelopmentTlsCertificate(certificatePem: String): X509Certificate {
    val encodedCertificate = certificatePemPattern.matchEntire(certificatePem)?.groupValues?.get(1)
        ?: throw GradleException("Development TLS certificate must contain exactly one CERTIFICATE PEM block")
    val certificateBytes = runCatching { Base64.getMimeDecoder().decode(encodedCertificate) }.getOrElse { error ->
        throw GradleException("Development TLS certificate is malformed", error)
    }
    val certificates = runCatching {
        CertificateFactory.getInstance("X.509").generateCertificates(ByteArrayInputStream(certificateBytes))
    }.getOrElse { error ->
        throw GradleException("Development TLS certificate is malformed", error)
    }
    if (certificates.size != 1) {
        throw GradleException("Development TLS certificate must contain exactly one certificate")
    }
    val certificate = certificates.single() as? X509Certificate
        ?: throw GradleException("Development TLS certificate is not an X.509 certificate")
    if (!certificate.encoded.contentEquals(certificateBytes)) {
        throw GradleException("Development TLS certificate contains unexpected data")
    }
    if (certificate.basicConstraints < 0) {
        throw GradleException("Development TLS certificate must be a CA certificate")
    }
    runCatching(certificate::checkValidity).getOrElse { error ->
        throw GradleException("Development TLS certificate is expired or not yet valid", error)
    }
    return certificate
}

fun developmentTlsPem(certificate: X509Certificate): String {
    val encodedCertificate = Base64.getMimeEncoder(64, byteArrayOf('\n'.code.toByte())).encodeToString(certificate.encoded)
    return "-----BEGIN CERTIFICATE-----\n$encodedCertificate\n-----END CERTIFICATE-----\n"
}

val developmentTlsEnabled = debugEnabled()
val developmentTlsCertificate = System.getenv("PEPPY_DEV_TLS_CERT")?.takeUnless(String::isBlank)?.let { configuredPath ->
    File(configuredPath).also { certificate ->
        if (!certificate.isAbsolute) {
            throw GradleException("PEPPY_DEV_TLS_CERT must be an absolute path")
        }
    }
}
val developmentTlsResources = layout.buildDirectory.dir("generated/res/devTls/debug")

// Operator configuration is deliberately local. Builds without google-services.json keep FCM
// optional and must not require or commit provider credentials.
if (file("google-services.json").isFile) {
    apply(plugin = "com.google.gms.google-services")
}

android {
    namespace = "dev.peppy.mobile"
    compileSdk = 36

    defaultConfig {
        applicationId = "dev.peppy.mobile"
        minSdk = 26
        targetSdk = 36
        val configuredVersionCode = providers.gradleProperty("peppyVersionCode").orNull ?: "1"
        versionCode = configuredVersionCode.toIntOrNull()?.takeIf { it in 1..2_100_000_000 }
            ?: throw GradleException(
                "peppyVersionCode must be an integer between 1 and 2100000000; got '$configuredVersionCode'"
            )
        versionName = providers.gradleProperty("peppyVersionName").orNull ?: "0.0.0-dev"
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
        val googleServerClientId = providers.gradleProperty("peppyGoogleServerClientId").orElse(
            providers.environmentVariable("PEPPY_GOOGLE_NATIVE_SERVER_CLIENT_ID"),
        ).orNull.orEmpty()
        buildConfigField("String", "PEPPY_GOOGLE_SERVER_CLIENT_ID", "\"${googleServerClientId.replace("\\", "\\\\").replace("\"", "\\\"")}\"")
        buildConfigField("boolean", "PEPPY_SMS_DIAGNOSTICS", "false")
        // Native bindings are verified and packaged for physical ARM64 and x86_64 emulators.
        ndk {
            abiFilters.clear()
            abiFilters.addAll(selectedAndroidAbi?.let(::setOf) ?: setOf("arm64-v8a", "x86_64"))
        }
    }

    buildFeatures {
        compose = true
        // BuildConfig.DEBUG gates the loopback-only cleartext development origin.
        buildConfig = true
    }

    buildTypes {
        getByName("debug") {
            buildConfigField("boolean", "PEPPY_SMS_DIAGNOSTICS", peppySmsDiagnostics.toString())
        }
        getByName("release") {
            buildConfigField("boolean", "PEPPY_SMS_DIAGNOSTICS", "false")
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    kotlinOptions { jvmTarget = "17" }

    sourceSets {
        getByName("main").jniLibs.srcDir("src/main/jniLibs")
        getByName("debug").res.srcDir(developmentTlsResources)
    }

    testOptions {
        unitTests {
            isIncludeAndroidResources = true
            all { test ->
                // Robolectric tests exercise the real generated bindings against the host build of
                // the same Rust crate (SQLCipher + libsodium); there is no mock message store.
                test.systemProperty(
                    "uniffi.component.peppy_mobile_bindings.libraryOverride",
                    rootProject.projectDir.resolve(
                        "../../target/debug/" + System.mapLibraryName("peppy_mobile_bindings")
                    ).canonicalPath,
                )
            }
        }
    }
}

dependencies {
    implementation(platform("androidx.compose:compose-bom:2025.10.01"))
    implementation("androidx.activity:activity-compose:1.10.0")
    implementation("androidx.work:work-runtime-ktx:2.10.0")
    implementation("com.squareup.okhttp3:okhttp:4.12.0")
    implementation("androidx.core:core-ktx:1.15.0")
    implementation("androidx.credentials:credentials:1.5.0")
    implementation("androidx.credentials:credentials-play-services-auth:1.5.0")
    implementation("com.google.android.libraries.identity.googleid:googleid:1.1.1")
    implementation("com.google.zxing:core:3.5.3")
    // Offline, on-device QR decoding. CameraX 1.4.2 supports minSdk 21/compileSdk 36;
    // ML Kit barcode-scanning 17.3.0 bundles the decoder and makes no cloud request.
    implementation("androidx.camera:camera-camera2:1.4.2")
    implementation("androidx.camera:camera-lifecycle:1.4.2")
    implementation("androidx.camera:camera-view:1.4.2")
    implementation("com.google.mlkit:barcode-scanning:17.3.0")
    // Optional at runtime without google-services.json; required to compile the content-free wake service.
    implementation("com.google.firebase:firebase-messaging:25.1.3")
    implementation("androidx.compose.material3:material3")
    implementation("androidx.compose.material:material-icons-extended")
    implementation("androidx.compose.ui:ui")
    implementation("androidx.compose.ui:ui-tooling-preview")
    debugImplementation("androidx.compose.ui:ui-tooling")
    // UniFFI-generated Kotlin uses JNA to load libpeppy_mobile_bindings.so
    // from this APK's jniLibs directory; never from an arbitrary filesystem path.
    implementation("net.java.dev.jna:jna:5.14.0@aar")

    testImplementation("junit:junit:4.13.2")
    testImplementation("org.robolectric:robolectric:4.14.1")
    testImplementation("androidx.test:core:1.6.1")
    testImplementation("androidx.work:work-testing:2.10.0")
    // Desktop JNA jar (with host jnidispatch) for the JVM-hosted Robolectric runtime.
    testImplementation("net.java.dev.jna:jna:5.14.0")

    androidTestImplementation("androidx.test:core:1.6.1")
    androidTestImplementation("androidx.test:runner:1.6.2")
    androidTestImplementation("androidx.test.ext:junit:1.2.1")
    androidTestImplementation(platform("androidx.compose:compose-bom:2025.10.01"))
    androidTestImplementation("androidx.compose.ui:ui-test-junit4")
    debugImplementation("androidx.compose.ui:ui-test-manifest")
}

val generateDebugNetworkSecurityConfig = tasks.register("generateDebugNetworkSecurityConfig") {
    inputs.property("developmentTlsEnabled", developmentTlsEnabled)
    if (developmentTlsEnabled && developmentTlsCertificate != null) {
        inputs.file(developmentTlsCertificate).optional()
    }
    outputs.dir(developmentTlsResources)

    doLast {
        val resourcesDirectory = developmentTlsResources.get().asFile
        val rawDirectory = resourcesDirectory.resolve("raw")
        val xmlDirectory = resourcesDirectory.resolve("xml")
        resourcesDirectory.deleteRecursively()
        xmlDirectory.mkdirs()

        val tlsTrustAnchors = if (developmentTlsEnabled) {
            val certificateFile = developmentTlsCertificate
                ?: throw GradleException("DEBUG=true requires PEPPY_DEV_TLS_CERT")
            if (!certificateFile.isFile) {
                throw GradleException("DEBUG=true requires CA certificate at ${certificateFile.path}")
            }
            val certificate = validatedDevelopmentTlsCertificate(certificateFile.readText())
            rawDirectory.mkdirs()
            rawDirectory.resolve("peppy_dev_tls_ca.pem").writeText(developmentTlsPem(certificate))
            """
                <trust-anchors>
                    <certificates src="system" />
                    <certificates src="@raw/peppy_dev_tls_ca" />
                </trust-anchors>
            """.trimIndent()
        } else {
            ""
        }

        xmlDirectory.resolve("network_security_config.xml").writeText(
            """
                <?xml version="1.0" encoding="utf-8"?>
                <network-security-config>
                    <base-config cleartextTrafficPermitted="false" />
                    <domain-config cleartextTrafficPermitted="true">
                        <domain includeSubdomains="false">127.0.0.1</domain>
                        <domain includeSubdomains="false">localhost</domain>
                        <domain includeSubdomains="false">::1</domain>
                        <domain includeSubdomains="false">10.0.2.2</domain>
                        $tlsTrustAnchors
                    </domain-config>
                </network-security-config>
            """.trimIndent().trimStart() + "\n",
        )
    }
}

val debugResourceConsumerTasks = setOf(
    "preDebugBuild",
    "mapDebugSourceSetPaths",
    "generateDebugResources",
    "packageDebugResources",
    "extractDeepLinksDebug",
    "parseDebugLocalResources",
    "mergeDebugResources",
    "processDebugResources",
    "preDebugAndroidTestBuild",
    "mapDebugAndroidTestSourceSetPaths",
    "generateDebugAndroidTestResources",
    "mergeDebugAndroidTestResources",
    "processDebugAndroidTestResources",
)

tasks.matching { it.name in debugResourceConsumerTasks }.configureEach {
    dependsOn(generateDebugNetworkSecurityConfig)
}

tasks.register("verifyDevTlsNetworkSecurityConfig") {
    dependsOn(generateDebugNetworkSecurityConfig)

    doLast {
        val generatedResources = layout.buildDirectory.dir("generated/res/devTls/debug").get().asFile
        val config = generatedResources.resolve("xml/network_security_config.xml")
        check(config.isFile) { "Debug builds must generate a network security configuration" }
        DocumentBuilderFactory.newInstance().newDocumentBuilder().parse(config)
        val configText = config.readText()
        val localDomains = listOf("127.0.0.1", "localhost", "::1", "10.0.2.2")
        check(localDomains.all { domain -> configText.contains(">$domain</domain>") }) {
            "Debug configuration must include every local development host"
        }
        val certificate = generatedResources.resolve("raw/peppy_dev_tls_ca.pem")
        if (developmentTlsEnabled) {
            check(certificate.isFile) { "Enabled development TLS must generate a debug-only CA resource" }
            check(configText.contains("<certificates src=\"system\" />")) {
                "Development TLS must retain system trust anchors"
            }
            check(configText.contains("<certificates src=\"@raw/peppy_dev_tls_ca\" />")) {
                "Development TLS must trust its debug-only CA resource"
            }
        } else {
            check(!certificate.exists()) { "Disabled development TLS must not generate a CA resource" }
        }
    }
}

tasks.register("verifyDevTlsCertificateValidation") {
    onlyIf { developmentTlsEnabled && developmentTlsCertificate?.isFile == true }

    doLast {
        val certificatePem = developmentTlsCertificate!!.readText()
        check(runCatching {
            validatedDevelopmentTlsCertificate(
                certificatePem + "-----BEGIN PRIVATE KEY-----\nAA==\n-----END PRIVATE KEY-----\n",
            )
        }.isFailure) { "Certificate and private-key PEM input must be rejected" }
        check(runCatching {
            validatedDevelopmentTlsCertificate(certificatePem.trimEnd() + "\n" + certificatePem)
        }.isFailure) { "Multiple certificate PEM blocks must be rejected" }
    }
}

tasks.register("verifyReleaseDevTlsResources") {
    dependsOn("processReleaseResources")

    doLast {
        val releaseResources = layout.buildDirectory.dir("intermediates/packaged_res/release/packageReleaseResources").get().asFile
        check(!releaseResources.resolve("raw/peppy_dev_tls_ca.pem").exists()) {
            "Release resources must not contain the development TLS CA"
        }
    }
}
