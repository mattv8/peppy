import java.io.ByteArrayInputStream
import java.security.cert.CertificateFactory
import java.security.cert.X509Certificate
import java.security.MessageDigest
import java.util.Base64
import java.util.zip.ZipFile
import java.util.zip.ZipEntry
import java.util.zip.ZipOutputStream
import javax.xml.parsers.DocumentBuilderFactory
import groovy.json.JsonSlurper

plugins {
    id("com.android.application")
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

fun strictBooleanProperty(name: String): Boolean = providers.gradleProperty(name).orNull?.let { value ->
    when (value) {
        "true" -> true
        "false" -> false
        else -> throw GradleException("$name must be true or false; got '$value'")
    }
} ?: false

val peppyHostedAuthDiagnostics = strictBooleanProperty("peppyHostedAuthDiagnostics")
val peppySideloadDebugSigning = strictBooleanProperty("peppySideloadDebugSigning")
val releaseTaskRequested = gradle.startParameter.taskNames.any { taskName ->
    taskName.contains("release", ignoreCase = true) || taskName.contains("sideload", ignoreCase = true)
}
require(!releaseTaskRequested || !peppyHostedAuthDiagnostics) {
    "peppyHostedAuthDiagnostics cannot be enabled for release or sideload tasks"
}
require(!providers.gradleProperty("peppySmsDiagnostics").isPresent) {
    "peppySmsDiagnostics was removed; enable diagnostics at runtime in Settings"
}

val peppyDistribution = providers.gradleProperty("peppyDistribution").orNull ?: "direct"
require(peppyDistribution in setOf("direct", "play")) {
    "peppyDistribution must be direct or play; got '$peppyDistribution'"
}
val jnaVersion = "5.19.1"

fun gitValueOrNull(vararg arguments: String): String? = runCatching {
    ProcessBuilder("git", *arguments).directory(rootProject.projectDir.resolve("../.."))
        .redirectErrorStream(true).start().let { process ->
            val value = process.inputStream.bufferedReader().readText().trim()
            value.takeIf { process.waitFor() == 0 }
        }
}.getOrNull()

fun validatedMetadata(value: String?, pattern: Regex, name: String): String = when (value) {
    null, "unknown" -> "unknown"
    else -> value.takeIf(pattern::matches) ?: throw GradleException("$name has an invalid format")
}

fun validatedVersionName(value: String): String {
    require(Regex("[0-9A-Za-z._+\\-]{1,64}").matches(value)) {
        "peppyVersionName must contain 1-64 ASCII letters, digits, dots, underscores, pluses, or hyphens"
    }
    return value
}

fun sourceDiffSha256(): String {
    val sourceRoot = rootProject.projectDir.resolve("../..").canonicalFile
    val digest = MessageDigest.getInstance("SHA-256")
    fun updateFrom(input: java.io.InputStream) {
        val buffer = ByteArray(DEFAULT_BUFFER_SIZE)
        while (true) {
            val count = input.read(buffer)
            if (count < 0) return
            digest.update(buffer, 0, count)
        }
    }
    return runCatching {
        ProcessBuilder("git", "diff", "--no-ext-diff", "--binary", "HEAD").directory(sourceRoot).start().also { process ->
            process.inputStream.use(::updateFrom)
            check(process.waitFor() == 0) { "git diff failed" }
        }
        val paths = ProcessBuilder("git", "ls-files", "-z", "--others", "--exclude-standard").directory(sourceRoot).start()
        val untracked = buildList {
            val path = StringBuilder()
            paths.inputStream.use { input ->
                while (true) {
                    val next = input.read()
                    if (next < 0) break
                    if (next == 0) { add(path.toString()); path.clear() } else path.append(next.toChar())
                }
            }
        }
        check(paths.waitFor() == 0) { "git ls-files failed" }
        untracked.forEach { path ->
            val file = sourceRoot.resolve(path).canonicalFile
            require(file.toPath().startsWith(sourceRoot.toPath()) && file.isFile) { "Invalid untracked build provenance path" }
            digest.update(path.toByteArray(Charsets.UTF_8)); file.inputStream().use(::updateFrom)
        }
        digest.digest().joinToString("") { "%02x".format(it.toInt() and 0xff) }
    }.getOrElse { "unknown" }
}

val nativeProfile = (providers.environmentVariable("PEPPY_ANDROID_NATIVE_PROFILE").orNull ?: "unknown").also { profile ->
    require(profile in setOf("unknown", "debug", "release")) { "PEPPY_ANDROID_NATIVE_PROFILE must be debug or release" }
}

val sourceCommit = validatedMetadata(providers.gradleProperty("peppySourceCommit").orNull ?: gitValueOrNull("rev-parse", "HEAD"), Regex("[0-9a-f]{40}"), "peppySourceCommit")
val sourceDirtyState = providers.gradleProperty("peppySourceDirty").orNull ?: gitValueOrNull("status", "--porcelain")?.let { if (it.isEmpty()) "false" else "true" } ?: "unknown"
require(sourceDirtyState in setOf("true", "false", "unknown")) { "peppySourceDirty must be true, false, or unknown" }
val sourceDiffSha256 = validatedMetadata(providers.gradleProperty("peppySourceDiffSha256").orNull ?: sourceDiffSha256(), Regex("[0-9a-f]{64}"), "peppySourceDiffSha256")

// Robolectric's own runtime download has no network timeout and can hang the test worker, so
// Gradle stages the framework jars and tests run Robolectric offline. These must match the SDKs
// in the tests' @Config annotations (26, 30, 31, 34, 35) for this Robolectric version; a missing jar
// fails the test immediately. Each needs its own configuration, or Gradle keeps only the newest.
val robolectricDependencyDirectory = layout.buildDirectory.dir("robolectric-dependencies")
val robolectricDependencies = listOf(
    "org.robolectric:android-all-instrumented:8.0.0_r4-robolectric-r1-i7",
    "org.robolectric:android-all-instrumented:11-robolectric-6757853-i7",
    "org.robolectric:android-all-instrumented:12-robolectric-7732740-i7",
    "org.robolectric:android-all-instrumented:14-robolectric-10818077-i7",
    "org.robolectric:android-all-instrumented:15-robolectric-13954326-i7",
).mapIndexed { index, coordinate ->
    configurations.create("robolectricDependency$index").also { configuration ->
        dependencies.add(configuration.name, coordinate)
    }
}
val prepareRobolectricDependencies = tasks.register<Sync>("prepareRobolectricDependencies") {
    from(robolectricDependencies)
    into(robolectricDependencyDirectory)
}

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
val developmentTlsResources = layout.buildDirectory.dir("generated/res/devTls/local")

// Operator configuration is deliberately local. Builds without google-services.json keep FCM
// optional and must not require or commit provider credentials.
if (file("google-services.json").isFile) {
    apply(plugin = "com.google.gms.google-services")
}

android {
    namespace = "dev.peppy.mobile"
    compileSdk = 37
    ndkVersion = "27.2.12479018"

    defaultConfig {
        applicationId = "dev.peppy.mobile"
        minSdk = 26
        targetSdk = 36
        val configuredVersionCode = providers.gradleProperty("peppyVersionCode").orNull ?: "1"
        versionCode = configuredVersionCode.toIntOrNull()?.takeIf { it in 1..2_100_000_000 }
            ?: throw GradleException(
                "peppyVersionCode must be an integer between 1 and 2100000000; got '$configuredVersionCode'"
            )
        versionName = validatedVersionName(providers.gradleProperty("peppyVersionName").orNull ?: "0.0.0-dev")
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
        val googleServerClientId = providers.gradleProperty("peppyGoogleServerClientId").orElse(
            providers.environmentVariable("PEPPY_GOOGLE_NATIVE_SERVER_CLIENT_ID"),
        ).orNull.orEmpty()
        buildConfigField("String", "PEPPY_GOOGLE_SERVER_CLIENT_ID", "\"${googleServerClientId.replace("\\", "\\\\").replace("\"", "\\\"")}\"")
        buildConfigField("boolean", "PEPPY_HOSTED_AUTH_DIAGNOSTICS", "false")
        buildConfigField("String", "PEPPY_SOURCE_COMMIT", "\"$sourceCommit\"")
        buildConfigField("boolean", "PEPPY_SOURCE_DIRTY", (sourceDirtyState == "true").toString())
        buildConfigField("String", "PEPPY_SOURCE_DIRTY_STATE", "\"$sourceDirtyState\"")
        buildConfigField("String", "PEPPY_SOURCE_DIFF_SHA256", "\"$sourceDiffSha256\"")
        buildConfigField("String", "PEPPY_NATIVE_PROFILE", "\"$nativeProfile\"")
        buildConfigField("String", "PEPPY_DISTRIBUTION", "\"$peppyDistribution\"")
        manifestPlaceholders["peppyDistribution"] = peppyDistribution
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
            buildConfigField("boolean", "PEPPY_HOSTED_AUTH_DIAGNOSTICS", peppyHostedAuthDiagnostics.toString())
        }
        getByName("release") {
            isDebuggable = false
            isMinifyEnabled = false
            buildConfigField("boolean", "PEPPY_HOSTED_AUTH_DIAGNOSTICS", "false")
            ndk { debugSymbolLevel = "SYMBOL_TABLE" }
        }
        create("sideload") {
            initWith(getByName("release"))
            matchingFallbacks += listOf("release")
            // CI intentionally leaves this unsigned for infra/release/sign-android-artifacts.sh.
            if (peppySideloadDebugSigning) {
                signingConfig = signingConfigs.getByName("debug")
            }
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    sourceSets {
        getByName("main").jniLibs.srcDir("src/main/jniLibs")
        getByName("debug").res.srcDir(developmentTlsResources.get().asFile)
        getByName("sideload").manifest.srcFile("src/sideload/AndroidManifest.xml")
    }

    testOptions {
        unitTests {
            isIncludeAndroidResources = true
            all { test ->
                // Robolectric tests exercise the real generated bindings against the host build of
                // the same Rust crate (SQLCipher + libsodium); there is no mock message store.
                test.dependsOn(prepareRobolectricDependencies)
                test.systemProperty("robolectric.offline", "true")
                test.systemProperty("robolectric.dependency.dir", robolectricDependencyDirectory.get().asFile.canonicalPath)
                val nativeLibrary = rootProject.projectDir.resolve(
                    "../../target/debug/" + System.mapLibraryName("peppy_mobile_bindings")
                ).canonicalFile
                test.systemProperty("uniffi.component.peppy_mobile_bindings.libraryOverride", nativeLibrary.path)
                // Track the library's contents so a native-only change can never reuse a cached test result.
                test.inputs.file(nativeLibrary)
                    .withPropertyName("peppyBindingsLibrary")
                    .withPathSensitivity(PathSensitivity.NONE)
                if (System.getenv("CI") == "true") {
                    test.testLogging {
                        events("started", "passed", "skipped", "failed")
                    }
                }
            }
        }
    }
}

androidComponents {
    beforeVariants(selector().withBuildType("release")) { variantBuilder ->
        (variantBuilder as com.android.build.api.variant.HasUnitTestBuilder).enableUnitTest = true
    }
    beforeVariants(selector().withBuildType("sideload")) { variantBuilder ->
        (variantBuilder as com.android.build.api.variant.HasUnitTestBuilder).enableUnitTest = true
    }
}

dependencies {
    implementation(platform("androidx.compose:compose-bom:2026.09.00"))
    implementation("androidx.activity:activity-compose:1.10.0")
    implementation("androidx.work:work-runtime-ktx:2.12.0")
    implementation("com.squareup.okhttp3:okhttp:5.5.1")
    implementation("androidx.core:core-ktx:1.19.1")
    implementation("androidx.credentials:credentials:1.5.0")
    implementation("androidx.credentials:credentials-play-services-auth:1.5.0")
    implementation("com.google.android.libraries.identity.googleid:googleid:1.2.1")
    implementation("com.google.zxing:core:3.5.4")
    // Offline, on-device QR decoding. CameraX 1.4.2 supports minSdk 21/compileSdk 36;
    // ML Kit barcode-scanning 17.3.0 bundles the decoder and makes no cloud request.
    implementation("androidx.camera:camera-camera2:1.4.2")
    implementation("androidx.camera:camera-lifecycle:1.4.2")
    implementation("androidx.camera:camera-view:1.4.2")
    implementation("com.google.mlkit:barcode-scanning:17.3.0")
    // Optional at runtime without google-services.json; required to compile the content-free wake service.
    implementation("com.google.firebase:firebase-messaging:26.0.1")
    implementation("androidx.compose.material3:material3")
    implementation("androidx.compose.material:material-icons-extended")
    implementation("androidx.compose.ui:ui")
    implementation("androidx.compose.ui:ui-tooling-preview")
    debugImplementation("androidx.compose.ui:ui-tooling")
    // UniFFI-generated Kotlin uses JNA to load libpeppy_mobile_bindings.so
    // from this APK's jniLibs directory; never from an arbitrary filesystem path.
    implementation("net.java.dev.jna:jna:$jnaVersion@aar")
    implementation("com.android.billingclient:billing:9.1.0")

    testImplementation("junit:junit:4.13.2")
    testImplementation("org.robolectric:robolectric:4.17")
    testImplementation("androidx.test:core:1.6.1")
    testImplementation("androidx.work:work-testing:2.12.0")
    testImplementation(platform("androidx.compose:compose-bom:2026.09.00"))
    testImplementation("androidx.compose.ui:ui-test-junit4")
    // Desktop JNA jar (with host jnidispatch) for the JVM-hosted Robolectric runtime.
    testImplementation("net.java.dev.jna:jna:$jnaVersion")

    androidTestImplementation("androidx.test:core:1.6.1")
    androidTestImplementation("androidx.test:runner:1.6.2")
    androidTestImplementation("androidx.test.ext:junit:1.2.1")
    androidTestImplementation(platform("androidx.compose:compose-bom:2026.09.00"))
    androidTestImplementation("androidx.compose.ui:ui-test-junit4")
    debugImplementation("androidx.compose.ui:ui-test-manifest")
    add("sideloadImplementation", project(":libadb"))
}

configurations.configureEach {
    if (name == "sideloadUnitTestRuntimeClasspath") {
        exclude(group = "org.bouncycastle", module = "bcprov-jdk18on")
        exclude(group = "org.bouncycastle", module = "bcpkix-jdk18on")
        exclude(group = "org.bouncycastle", module = "bcutil-jdk18on")
    }
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
        val generatedResources = layout.buildDirectory.dir("generated/res/devTls/local").get().asFile
        val config = generatedResources.resolve("xml/network_security_config.xml")
        check(config.isFile) { "Local development builds must generate a network security configuration" }
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

val androidNamespace = "http://schemas.android.com/apk/res/android"
val componentElementNames = setOf("activity", "activity-alias", "service", "receiver", "provider")
val permittedReleaseExports = setOf(
    Triple("activity", "dev.peppy.mobile.MainActivity", null),
    Triple("receiver", "dev.peppy.mobile.IncomingSmsReceiver", "android.permission.BROADCAST_SMS"),
    Triple("receiver", "dev.peppy.mobile.MmsNotificationReceiver", "android.permission.BROADCAST_WAP_PUSH"),
    Triple(
        "service",
        "com.google.android.gms.auth.api.signin.RevocationBoundService",
        "com.google.android.gms.auth.api.signin.permission.REVOCATION_NOTIFICATION",
    ),
    Triple("receiver", "com.google.firebase.iid.FirebaseInstanceIdReceiver", "com.google.android.c2dm.permission.SEND"),
    Triple("service", "androidx.work.impl.background.systemjob.SystemJobService", "android.permission.BIND_JOB_SERVICE"),
    Triple(
        "receiver",
        "androidx.work.impl.diagnostics.DiagnosticsReceiver",
        "android.permission.DUMP",
    ),
    Triple("receiver", "androidx.profileinstaller.ProfileInstallReceiver", "android.permission.DUMP"),
)

fun verifyManifestSecurity(mergedManifest: File, label: String) {
        check(mergedManifest.isFile) { "Expected merged $label manifest at $mergedManifest" }
        val parserFactory = DocumentBuilderFactory.newInstance().apply { isNamespaceAware = true }
        val manifest = parserFactory.newDocumentBuilder().parse(mergedManifest)
        val application = manifest.getElementsByTagName("application").item(0) as? org.w3c.dom.Element
            ?: throw GradleException("Merged $label manifest has no application element")
        check(application.getAttributeNS(androidNamespace, "debuggable").ifEmpty { "false" } == "false") {
            "$label manifest must set android:debuggable=false"
        }
        check(application.getAttributeNS(androidNamespace, "usesCleartextTraffic").ifEmpty { "false" } == "false") {
            "$label manifest must deny cleartext traffic"
        }
        check(application.getAttributeNS(androidNamespace, "networkSecurityConfig") == "@xml/network_security_config") {
            "$label manifest must use the restrictive network security configuration"
        }
        val networkConfig = file("src/main/res/xml/network_security_config.xml")
        val networkConfigDocument = parserFactory.newDocumentBuilder().parse(networkConfig)
        val baseConfig = networkConfigDocument.getElementsByTagName("base-config").item(0)
            ?: throw GradleException("Release network security configuration has no base-config")
        check(baseConfig.attributes.getNamedItem("cleartextTrafficPermitted")?.nodeValue == "false") {
            "$label network security configuration must deny cleartext traffic"
        }
        val exportedComponents = buildSet {
            val childNodes = application.childNodes
            for (index in 0 until childNodes.length) {
                val component = childNodes.item(index)
                val exported = component.attributes?.getNamedItemNS(androidNamespace, "exported")?.nodeValue
                if (component.nodeName in componentElementNames && exported == "true") {
                    add(Triple(
                        component.nodeName,
                        component.attributes.getNamedItemNS(androidNamespace, "name")?.nodeValue,
                        component.attributes.getNamedItemNS(androidNamespace, "permission")?.nodeValue,
                    ))
                }
            }
        }
        check(exportedComponents == permittedReleaseExports) {
            "$label manifest exported components differ from the reviewed allowlist: $exportedComponents"
        }
}

val verifyReleaseManifestSecurity = tasks.register("verifyReleaseManifestSecurity") {
    dependsOn("processReleaseManifest")

    doLast {
        val mergedManifest = layout.buildDirectory
            .file("intermediates/merged_manifests/release/processReleaseManifest/AndroidManifest.xml")
            .get()
            .asFile
        verifyManifestSecurity(mergedManifest, "Release")
    }
}

tasks.matching { it.name in setOf("assembleRelease", "bundleRelease") }.configureEach {
    dependsOn(verifyReleaseManifestSecurity)
}

val restrictedSetupPackagePath = "dev/peppy/mobile/restrictedsetup/"
val restrictedSetupDexPrefix = "L$restrictedSetupPackagePath"
val restrictedSetupDexPrefixes = setOf(restrictedSetupDexPrefix, "Lio/github/muntashirakon/")
val permittedRestrictedSetupDexClasses = Regex(
    "${Regex.escape(restrictedSetupDexPrefix)}RestrictedSetupEntryKt(?:\\${'$'}\\${'$'}ExternalSyntheticLambda\\d+)?;",
)
val restrictedSetupDependencyGroups = setOf(
    "com.github.MuntashirAkon",
    "io.github.muntashirakon",
    "org.conscrypt",
    "org.bouncycastle",
)
// Extend these deny lists before approving any future restricted-setup transport dependency.
val restrictedSetupDependencyCoordinates = setOf(
    "com.github.MuntashirAkon:libadb-android",
    "com.github.MuntashirAkon:spake2-android",
    "org.conscrypt:conscrypt-android",
)
val restrictedSetupNativeLibraryNames = setOf("libadb", "libspake2.so", "libconscrypt_jni.so")

fun apksFromMetadata(metadataFile: File, variant: String): List<File> {
    check(metadataFile.isFile) { "Expected $variant APK metadata at $metadataFile" }
    val metadata = JsonSlurper().parse(metadataFile) as? Map<*, *>
        ?: throw GradleException("$variant APK metadata must be an object")
    check(metadata["variantName"] == variant) { "$variant APK metadata has an unexpected variant" }
    val artifactType = metadata["artifactType"] as? Map<*, *>
    check(artifactType?.get("type") == "APK") { "$variant APK metadata does not describe APK outputs" }
    val outputDirectory = metadataFile.parentFile.canonicalFile
    val outputs = (metadata["elements"] as? List<*>)
        ?.map { it as? Map<*, *> ?: throw GradleException("$variant APK metadata has an invalid output") }
        ?.map { it["outputFile"] as? String ?: throw GradleException("$variant APK metadata has an output without a filename") }
        ?.map { outputFile -> outputDirectory.resolve(outputFile).canonicalFile }
        ?: emptyList()
    check(outputs.isNotEmpty()) { "$variant APK metadata contains no APK outputs" }
    check(outputs.all { it.toPath().startsWith(outputDirectory.toPath()) && it.isFile }) {
        "$variant APK metadata references a missing or unsafe APK output"
    }
    return outputs
}

fun restrictedSetupDexClasses(apks: List<File>): Set<String> = buildSet {
    val classDescriptor = Regex("L(?:${restrictedSetupDexPrefixes.joinToString("|") { Regex.escape(it.removePrefix("L")) }})[^;]+;")
    apks.forEach { apk ->
        ZipFile(apk).use { archive ->
            val entries = archive.entries()
            while (entries.hasMoreElements()) {
                val entry = entries.nextElement()
                if (entry.name.matches(Regex("(?:base/)?dex/classes(\\d+)?\\.dex")) || entry.name.matches(Regex("classes(\\d+)?\\.dex"))) {
                    classDescriptor.findAll(archive.getInputStream(entry).readBytes().toString(Charsets.ISO_8859_1))
                        .forEach { add(it.value) }
                }
            }
        }
    }
}

fun restrictedSetupNativeLibraryPaths(apks: List<File>): Set<String> = apks.flatMap { apk ->
    ZipFile(apk).use { archive ->
        archive.entries().asSequence()
            .map(ZipEntry::getName)
            .filter { it.matches(Regex("(?:base/)?lib/[^/]+/[^/]+")) }
            .filter { path -> restrictedSetupNativeLibraryNames.any(path.substringAfterLast('/').lowercase()::contains) }
            .toList()
    }
}.toSet()

fun restrictedSetupNativeLibraries(archives: List<File>): Set<String> =
    restrictedSetupNativeLibraryPaths(archives).map { it.substringAfterLast('/').lowercase() }.toSet()

fun archiveDexContains(archives: List<File>, value: String): Boolean = archives.any { archiveFile ->
    ZipFile(archiveFile).use { archive ->
        archive.entries().asSequence().filter { entry ->
            entry.name.matches(Regex("(?:base/)?dex/classes(\\d+)?\\.dex")) || entry.name.matches(Regex("classes(\\d+)?\\.dex"))
        }.any { entry ->
            archive.getInputStream(entry).use { input -> input.readBytes().toString(Charsets.ISO_8859_1).contains(value) }
        }
    }
}

fun archiveEntryContains(archives: List<File>, entryName: String, value: String): Boolean = archives.any { archiveFile ->
    ZipFile(archiveFile).use { archive ->
        archive.getEntry(entryName)?.let { entry ->
            archive.getInputStream(entry).use { input -> input.readBytes().toString(Charsets.ISO_8859_1).contains(value) }
        } ?: false
    }
}

fun archiveContainsEntryPrefix(archives: List<File>, prefix: String): Boolean = archives.any { archiveFile ->
    ZipFile(archiveFile).use { archive -> archive.entries().asSequence().any { it.name.startsWith(prefix) } }
}

fun archiveContainsEntry(archives: List<File>, name: String): Boolean = archives.any { archiveFile ->
    ZipFile(archiveFile).use { archive -> archive.getEntry(name) != null }
}

fun verifyRestrictedSetupManifest(mergedManifest: File, variant: String) {
    check(mergedManifest.isFile) { "Expected merged $variant manifest at $mergedManifest" }
    val parserFactory = DocumentBuilderFactory.newInstance().apply { isNamespaceAware = true }
    val manifest = parserFactory.newDocumentBuilder().parse(mergedManifest)
    val components = manifest.getElementsByTagName("*")
    for (index in 0 until components.length) {
        val component = components.item(index) as? org.w3c.dom.Element ?: continue
        if (component.nodeName !in componentElementNames) continue
        val rawName = component.getAttributeNS(androidNamespace, "name")
        val name = when {
            rawName.startsWith(".") -> "dev.peppy.mobile$rawName"
            rawName.contains('.') -> rawName
            else -> "dev.peppy.mobile.$rawName"
        }
        check(!name.startsWith("dev.peppy.mobile.restrictedsetup.")) {
            "$variant manifest must not contain restricted setup component $name"
        }
    }
    val restrictedPermissions = setOf(
        "android.permission.FOREGROUND_SERVICE_CONNECTED_DEVICE",
        "android.permission.CHANGE_NETWORK_STATE",
    )
    val permissions = manifest.getElementsByTagName("uses-permission")
    for (index in 0 until permissions.length) {
        val permission = permissions.item(index) as? org.w3c.dom.Element ?: continue
        check(permission.getAttributeNS(androidNamespace, "name") !in restrictedPermissions) {
            "$variant manifest must not contain restricted setup permission ${permission.getAttributeNS(androidNamespace, "name")}"
        }
    }
}

fun verifySideloadManifestSecurity(mergedManifest: File) {
    val parserFactory = DocumentBuilderFactory.newInstance().apply { isNamespaceAware = true }
    val manifest = parserFactory.newDocumentBuilder().parse(mergedManifest)
    verifyManifestSecurity(mergedManifest, "Sideload")
    val permissions = (0 until manifest.getElementsByTagName("uses-permission").length).asSequence()
        .map { manifest.getElementsByTagName("uses-permission").item(it) as? org.w3c.dom.Element }
        .filterNotNull()
        .map { it.getAttributeNS(androidNamespace, "name") }
        .toSet()
    val requiredPermissions = setOf(
        "android.permission.FOREGROUND_SERVICE_CONNECTED_DEVICE",
        "android.permission.CHANGE_NETWORK_STATE",
    )
    val releaseManifest = layout.buildDirectory
        .file("intermediates/merged_manifests/release/processReleaseManifest/AndroidManifest.xml").get().asFile
    val releasePermissions = parserFactory.newDocumentBuilder().parse(releaseManifest)
        .getElementsByTagName("uses-permission").let { nodes ->
            (0 until nodes.length).map { nodes.item(it) as? org.w3c.dom.Element }
                .filterNotNull()
                .map { it.getAttributeNS(androidNamespace, "name") }
                .toSet()
        }
    check(permissions - releasePermissions == requiredPermissions) {
        "Sideload manifest must add exactly the reviewed restricted setup permissions"
    }
    val restrictedComponents = (0 until manifest.getElementsByTagName("*").length).asSequence()
        .map { manifest.getElementsByTagName("*").item(it) as? org.w3c.dom.Element }
        .filterNotNull()
        .filter { it.nodeName in componentElementNames }
        .filter { it.getAttributeNS(androidNamespace, "name").startsWith("dev.peppy.mobile.restrictedsetup.") }
        .associateBy { it.getAttributeNS(androidNamespace, "name") }
    val serviceName = "dev.peppy.mobile.restrictedsetup.RestrictedSetupService"
    val receiverName = "dev.peppy.mobile.restrictedsetup.RestrictedSetupReplyReceiver"
    val service = restrictedComponents[serviceName]
    val receiver = restrictedComponents[receiverName]
    check(restrictedComponents.keys == setOf(serviceName, receiverName)) {
        "Sideload manifest must contain only the reviewed restricted setup components"
    }
    check(service?.nodeName == "service" && service.getAttributeNS(androidNamespace, "exported") == "false" &&
        service.getAttributeNS(androidNamespace, "foregroundServiceType") == "connectedDevice") {
        "Sideload restricted setup service must be non-exported and connectedDevice"
    }
    check(receiver?.nodeName == "receiver" && receiver.getAttributeNS(androidNamespace, "exported") == "false") {
        "Sideload restricted setup receiver must be non-exported"
    }
}

fun verifyRestrictedSetupExclusionTask(variant: String) = tasks.register(
    "verify${variant.replaceFirstChar(Char::uppercase)}RestrictedSetupExclusion",
) {
    dependsOn("package${variant.replaceFirstChar(Char::uppercase)}")

    doLast {
        val metadata = layout.buildDirectory.file("outputs/apk/$variant/output-metadata.json").get().asFile
        val apks = apksFromMetadata(metadata, variant)
        val prohibitedClasses = restrictedSetupDexClasses(apks).filterNot(permittedRestrictedSetupDexClasses::matches)
        check(prohibitedClasses.isEmpty()) {
            "$variant DEX must not contain development restricted setup classes: $prohibitedClasses"
        }
        val mergedManifest = layout.buildDirectory
            .file("intermediates/merged_manifests/$variant/process${variant.replaceFirstChar(Char::uppercase)}Manifest/AndroidManifest.xml")
            .get()
            .asFile
        verifyRestrictedSetupManifest(mergedManifest, variant)
        val prohibitedDependencies = configurations.getByName("${variant}RuntimeClasspath")
            .resolvedConfiguration.resolvedArtifacts
            .map { artifact -> Triple(artifact.moduleVersion.id.group, "${artifact.moduleVersion.id.group}:${artifact.name}", artifact.id.componentIdentifier.displayName) }
            .filter { (group, coordinate, component) ->
                group in restrictedSetupDependencyGroups || coordinate in restrictedSetupDependencyCoordinates || component == "project :libadb"
            }
            .toSet()
        check(prohibitedDependencies.isEmpty()) {
            "$variant runtime dependencies must not contain restricted setup transport groups: $prohibitedDependencies"
        }
        val prohibitedNativeLibraries = restrictedSetupNativeLibraries(apks)
        check(prohibitedNativeLibraries.isEmpty()) {
            "$variant APK must not contain restricted setup transport native libraries: $prohibitedNativeLibraries"
        }
        check(!archiveContainsEntryPrefix(apks, "assets/third_party/")) {
            "$variant APK must not contain sideload third-party notices"
        }
    }
}

fun verifyReleaseRestrictedSetupAabExclusion() {
    val releaseAab = layout.buildDirectory.file("outputs/bundle/release/app-release.aab").get().asFile
    check(releaseAab.isFile) { "Expected release AAB at $releaseAab" }
    val archives = listOf(releaseAab)
    val prohibitedClasses = restrictedSetupDexClasses(archives).filterNot(permittedRestrictedSetupDexClasses::matches)
    check(prohibitedClasses.isEmpty()) { "Release AAB DEX must not contain restricted setup classes: $prohibitedClasses" }
    check(restrictedSetupNativeLibraries(archives).isEmpty()) {
        "Release AAB must not contain restricted setup transport native libraries"
    }
    check(!archiveContainsEntryPrefix(archives, "base/assets/third_party/")) {
        "Release AAB must not contain sideload third-party notices"
    }
    check(!archiveEntryContains(archives, "base/manifest/AndroidManifest.xml", "dev.peppy.mobile.restrictedsetup") &&
        !archiveEntryContains(archives, "base/manifest/AndroidManifest.xml", "android.permission.FOREGROUND_SERVICE_CONNECTED_DEVICE") &&
        !archiveEntryContains(archives, "base/manifest/AndroidManifest.xml", "android.permission.CHANGE_NETWORK_STATE")) {
        "Release AAB manifest must not contain restricted setup content"
    }
}

val verifyReleaseRestrictedSetupExclusion = verifyRestrictedSetupExclusionTask("release")
val verifyDebugRestrictedSetupExclusion = verifyRestrictedSetupExclusionTask("debug")

val verifySideloadRestrictedSetupInclusion = tasks.register("verifySideloadRestrictedSetupInclusion") {
    dependsOn("packageSideload", "processSideloadManifest", "processReleaseManifest")

    doLast {
        val metadata = layout.buildDirectory.file("outputs/apk/sideload/output-metadata.json").get().asFile
        val apks = apksFromMetadata(metadata, "sideload")
        check(restrictedSetupDexClasses(apks).any { it.contains("RestrictedSetupService") }) {
            "Sideload APK must contain RestrictedSetupService"
        }
        check(archiveDexContains(apks, "Lio/github/muntashirakon/adb/")) {
            "Sideload APK must contain libadb classes"
        }
        val nativeLibraryPaths = restrictedSetupNativeLibraryPaths(apks)
        val expectedAbis = selectedAndroidAbi?.let(::setOf) ?: setOf("arm64-v8a", "x86_64")
        expectedAbis.forEach { abi ->
            check(listOf("libspake2.so", "libconscrypt_jni.so").all { library -> "lib/$abi/$library" in nativeLibraryPaths }) {
                "Sideload APK must contain libspake2.so and libconscrypt_jni.so for $abi"
            }
        }
        val requiredNotices = setOf("NOTICE.txt", "LGPL-3.0.txt", "LGPL-2.1.txt", "GPL-3.0.txt", "BoringSSL-LICENSE.txt")
        check(requiredNotices.all { notice -> archiveContainsEntry(apks, "assets/third_party/$notice") }) {
            "Sideload APK must contain required third-party notices"
        }
    }
}

val verifySideloadManifestSecurity = tasks.register("verifySideloadManifestSecurity") {
    dependsOn("processSideloadManifest", "processReleaseManifest")

    doLast {
        val sideloadManifest = layout.buildDirectory
            .file("intermediates/merged_manifests/sideload/processSideloadManifest/AndroidManifest.xml").get().asFile
        verifySideloadManifestSecurity(sideloadManifest)
    }
}

val verifyReleaseRestrictedSetupExclusionFixtures = tasks.register("verifyReleaseRestrictedSetupExclusionFixtures") {
    doLast {
        val fixtureDirectory = layout.buildDirectory.dir("tmp/restricted-setup-exclusion-fixtures").get().asFile
        fixtureDirectory.deleteRecursively()
        fixtureDirectory.mkdirs()
        val fixtureApk = fixtureDirectory.resolve("app-release-unsigned.apk")
        ZipOutputStream(fixtureApk.outputStream()).use { archive ->
            archive.putNextEntry(ZipEntry("classes.dex"))
            archive.write("${restrictedSetupDexPrefix}RestrictedSetupController;".toByteArray())
            archive.closeEntry()
        }
        val metadata = fixtureDirectory.resolve("output-metadata.json")
        metadata.writeText("""{"artifactType":{"type":"APK"},"variantName":"release","elements":[{"outputFile":"app-release-unsigned.apk"}]}""")
        check(runCatching { restrictedSetupDexClasses(apksFromMetadata(metadata, "release")) }.isSuccess)
        check(runCatching {
            check(restrictedSetupDexClasses(apksFromMetadata(metadata, "release")).all(permittedRestrictedSetupDexClasses::matches))
        }.isFailure) { "Synthetic development class must fail the release DEX exclusion guard" }

        val manifest = fixtureDirectory.resolve("AndroidManifest.xml")
        manifest.writeText("""<manifest xmlns:android="$androidNamespace"><application><service android:name="dev.peppy.mobile.restrictedsetup.RestrictedSetupController" /></application></manifest>""")
        check(runCatching { verifyRestrictedSetupManifest(manifest, "release") }.isFailure) {
            "Synthetic restricted setup component must fail the release manifest exclusion guard"
        }

        ZipOutputStream(fixtureApk.outputStream()).use { archive ->
            archive.putNextEntry(ZipEntry("lib/arm64-v8a/libadb.so"))
            archive.closeEntry()
        }
        check(restrictedSetupNativeLibraries(apksFromMetadata(metadata, "release")).isNotEmpty()) {
            "Synthetic restricted setup native library must be detected"
        }
        ZipOutputStream(fixtureApk.outputStream()).use { archive ->
            archive.putNextEntry(ZipEntry("assets/third_party/NOTICE.txt"))
            archive.closeEntry()
        }
        check(archiveContainsEntryPrefix(apksFromMetadata(metadata, "release"), "assets/third_party/")) {
            "Synthetic sideload notice must fail the release APK exclusion guard"
        }

        val fixtureAab = fixtureDirectory.resolve("app-release.aab")
        ZipOutputStream(fixtureAab.outputStream()).use { archive ->
            archive.putNextEntry(ZipEntry("base/dex/classes.dex"))
            archive.write("Lio/github/muntashirakon/adb/AdbManager;".toByteArray())
            archive.closeEntry()
            archive.putNextEntry(ZipEntry("base/lib/arm64-v8a/libspake2.so"))
            archive.closeEntry()
            archive.putNextEntry(ZipEntry("base/assets/third_party/NOTICE.txt"))
            archive.closeEntry()
        }
        check(restrictedSetupDexClasses(listOf(fixtureAab)).isNotEmpty()) {
            "Synthetic libadb class must fail the release AAB DEX exclusion guard"
        }
        check(restrictedSetupNativeLibraries(listOf(fixtureAab)).contains("libspake2.so")) {
            "Synthetic restricted setup native library must fail the release AAB exclusion guard"
        }
        check(archiveContainsEntryPrefix(listOf(fixtureAab), "base/assets/third_party/")) {
            "Synthetic sideload notice must fail the release AAB exclusion guard"
        }

        ZipOutputStream(fixtureApk.outputStream()).use { archive ->
            archive.putNextEntry(ZipEntry("classes.dex"))
            archive.write("${restrictedSetupDexPrefix}RestrictedSetupEntryKt;${restrictedSetupDexPrefix}RestrictedSetupEntryKt${'$'}${'$'}ExternalSyntheticLambda0;".toByteArray())
            archive.closeEntry()
        }
        check(restrictedSetupDexClasses(apksFromMetadata(metadata, "release")).all(permittedRestrictedSetupDexClasses::matches)) {
            "Synthetic no-op entry classes must remain allowed"
        }

        val sideloadFixture = fixtureDirectory.resolve("app-sideload-unsigned.apk")
        ZipOutputStream(sideloadFixture.outputStream()).use { archive ->
            archive.putNextEntry(ZipEntry("classes.dex"))
            archive.write("${restrictedSetupDexPrefix}RestrictedSetupService;Lio/github/muntashirakon/adb/AdbManager;".toByteArray())
            archive.closeEntry()
            for (library in listOf("libspake2.so", "libconscrypt_jni.so")) {
                archive.putNextEntry(ZipEntry("lib/arm64-v8a/$library"))
                archive.closeEntry()
            }
        }
        val sideloadArchives = listOf(sideloadFixture)
        check(restrictedSetupDexClasses(sideloadArchives).any { it.contains("RestrictedSetupService") })
        check(archiveDexContains(sideloadArchives, "Lio/github/muntashirakon/adb/"))
        check(setOf("libspake2.so", "libconscrypt_jni.so").all(restrictedSetupNativeLibraries(sideloadArchives)::contains))

        ZipOutputStream(sideloadFixture.outputStream()).use { archive ->
            archive.putNextEntry(ZipEntry("classes.dex"))
            archive.write("${restrictedSetupDexPrefix}RestrictedSetupService;".toByteArray())
            archive.closeEntry()
        }
        check(!archiveDexContains(listOf(sideloadFixture), "Lio/github/muntashirakon/adb/")) {
            "Synthetic sideload fixture without libadb classes must fail inclusion"
        }
    }
}

tasks.matching { it.name == "assembleRelease" }.configureEach {
    dependsOn(verifyReleaseRestrictedSetupExclusion)
}

tasks.matching { it.name == "bundleRelease" }.configureEach {
    dependsOn(verifyReleaseRestrictedSetupExclusion)
    doLast { verifyReleaseRestrictedSetupAabExclusion() }
}

tasks.matching { it.name == "assembleDebug" }.configureEach {
    dependsOn(verifyDebugRestrictedSetupExclusion)
}

tasks.matching { it.name == "assembleSideload" }.configureEach {
    dependsOn(verifySideloadRestrictedSetupInclusion, verifySideloadManifestSecurity)
}

gradle.projectsEvaluated {
    setOf("testSideloadUnitTest", "lintSideload", "assembleSideload").forEach { taskName ->
        check(tasks.names.contains(taskName)) { "Expected sideload task $taskName" }
    }
}

tasks.matching { it.name == "check" }.configureEach {
    dependsOn(verifyReleaseRestrictedSetupExclusionFixtures)
}
