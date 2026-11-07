import java.io.ByteArrayInputStream
import java.security.cert.CertificateFactory
import java.security.cert.X509Certificate
import java.security.MessageDigest
import java.util.Base64
import java.util.zip.ZipFile
import java.util.zip.ZipEntry
import java.util.zip.ZipOutputStream
import java.util.zip.CRC32
import javax.xml.parsers.DocumentBuilderFactory
import groovy.json.JsonSlurper

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

fun strictBooleanProperty(name: String): Boolean = providers.gradleProperty(name).orNull?.let { value ->
    when (value) {
        "true" -> true
        "false" -> false
        else -> throw GradleException("$name must be true or false; got '$value'")
    }
} ?: false

fun resolveGoogleMessagesLink(distribution: String, explicitValue: String?): Boolean {
    val explicit = explicitValue?.let { value ->
        when (value) {
            "true" -> true
            "false" -> false
            else -> throw GradleException("peppyGoogleMessagesLink must be true or false; got '$value'")
        }
    }
    if (distribution == "play") {
        require(explicit != true) { "peppyGoogleMessagesLink cannot be enabled for Play distribution" }
        return false
    }
    return explicit ?: (distribution == "direct")
}

val peppyHostedAuthDiagnostics = strictBooleanProperty("peppyHostedAuthDiagnostics")
val peppySideloadDebugSigning = strictBooleanProperty("peppySideloadDebugSigning")
val googleMessagesAar = rootProject.layout.projectDirectory.file("googlemessages/build/gmlink.aar")
val googleMessagesProvenance = rootProject.layout.projectDirectory.file("googlemessages/build/gmlink.aar.provenance")
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
val peppyGoogleMessagesLink = resolveGoogleMessagesLink(
    peppyDistribution,
    providers.gradleProperty("peppyGoogleMessagesLink").orNull,
)

gradle.taskGraph.whenReady {
    if (peppyDistribution == "play") {
        allTasks.firstOrNull { task -> task.name.contains("Sideload") }?.let { task ->
            throw GradleException("Play distribution cannot execute sideload task ${task.path}")
        }
    }
}
val jnaVersion = "5.17.0"

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

fun sha256(file: File): String = file.inputStream().use { input ->
    val digest = MessageDigest.getInstance("SHA-256")
    val buffer = ByteArray(DEFAULT_BUFFER_SIZE)
    while (true) {
        val count = input.read(buffer)
        if (count < 0) break
        digest.update(buffer, 0, count)
    }
    digest.digest().joinToString("") { "%02x".format(it.toInt() and 0xff) }
}

fun googleMessagesBuildInputsSha256(
    googleMessagesDirectory: File = rootProject.projectDir.resolve("googlemessages"),
): String {
    val inputs = googleMessagesDirectory.resolve("gmlink").listFiles()
        .orEmpty()
        .filter { it.isFile && it.extension == "go" }
        .sortedBy(File::getName) + listOf(
        googleMessagesDirectory.resolve("go.mod"),
        googleMessagesDirectory.resolve("go.sum"),
    )
    require(inputs.all(File::isFile)) { "Google Messages link build inputs are missing" }
    val content = inputs.joinToString("") { "${sha256(it)}  ${it.relativeTo(googleMessagesDirectory).path}\n" }
    return MessageDigest.getInstance("SHA-256").digest(content.toByteArray()).joinToString("") { "%02x".format(it.toInt() and 0xff) }
}

fun googleMessagesSafetyPatchSha256(googleMessagesDirectory: File): String {
    val patch = googleMessagesDirectory.resolve("upstream-safety.patch")
    val checksum = googleMessagesDirectory.resolve("upstream-safety.patch.sha256").readText()
        .trim()
        .split(Regex("\\s+"), limit = 2)
        .first()
    require(Regex("[0-9a-f]{64}").matches(checksum)) {
        "Google Messages safety patch checksum is malformed"
    }
    val actual = sha256(patch)
    check(actual == checksum) {
        "Google Messages safety patch checksum does not match upstream-safety.patch"
    }
    return actual
}

fun googleMessagesProvenanceInputs(
    googleMessagesDirectory: File = rootProject.projectDir.resolve("googlemessages"),
    aar: File = googleMessagesAar.asFile,
): Map<String, String> {
    val moduleVersion = googleMessagesDirectory.resolve("go.mod").readLines()
        .firstOrNull { it.trimStart().startsWith("go.mau.fi/mautrix-gmessages ") }
        ?.trim()
        ?.substringAfter("go.mau.fi/mautrix-gmessages ")
        ?: throw GradleException("Google Messages link libgm version is missing")
    return mapOf(
        "aar_sha256" to sha256(aar),
        "libgm_version" to moduleVersion,
        "safety_patch_sha256" to googleMessagesSafetyPatchSha256(googleMessagesDirectory),
        "inputs_sha256" to googleMessagesBuildInputsSha256(googleMessagesDirectory),
    )
}

fun verifyGoogleMessagesAarProvenance(
    googleMessagesDirectory: File = rootProject.projectDir.resolve("googlemessages"),
    aar: File = googleMessagesAar.asFile,
    provenanceFile: File = googleMessagesProvenance.asFile,
) {
    check(provenanceFile.isFile) {
        "peppyGoogleMessagesLink=true needs AAR provenance from apps/android/googlemessages/build-aar.sh"
    }
    val provenance = provenanceFile.readLines().associate { line ->
        val (name, value) = line.split('=', limit = 2).let { fields ->
            require(fields.size == 2 && fields[0].matches(Regex("[a-z0-9_]+"))) { "Google Messages AAR provenance is malformed" }
            fields[0] to fields[1]
        }
        name to value
    }
    val expected = googleMessagesProvenanceInputs(googleMessagesDirectory, aar)
    check(provenance == expected) { "Google Messages AAR provenance does not match current inputs" }
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
    "org.robolectric:android-all-instrumented:15-robolectric-12650502-i7",
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
    compileSdk = 36
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
    kotlinOptions { jvmTarget = "17" }

    sourceSets {
        getByName("main").jniLibs.srcDir("src/main/jniLibs")
        getByName("debug").res.srcDir(developmentTlsResources)
        getByName("sideload").manifest.srcFile("src/sideload/AndroidManifest.xml")
        getByName("sideload").java.srcDir(if (peppyGoogleMessagesLink) "src/sideloadGmlinkOn/java" else "src/sideloadGmlinkOff/java")
        if (peppyGoogleMessagesLink) getByName("sideload").assets.srcDir("src/sideloadGmlinkOn/assets")
    }

    testOptions {
        unitTests {
            isIncludeAndroidResources = true
            all { test ->
                // Robolectric tests exercise the real generated bindings against the host build of
                // the same Rust crate (SQLCipher + libsodium); there is no mock message store.
                test.dependsOn(prepareRobolectricDependencies)
                test.systemProperty("robolectric.offline", "true")
                test.systemProperty("peppyGoogleMessagesLink", peppyGoogleMessagesLink.toString())
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
    implementation("net.java.dev.jna:jna:$jnaVersion@aar")
    implementation("com.android.billingclient:billing:8.3.0")

    testImplementation("junit:junit:4.13.2")
    testImplementation("org.robolectric:robolectric:4.14.1")
    testImplementation("androidx.test:core:1.6.1")
    testImplementation("androidx.work:work-testing:2.10.0")
    testImplementation(platform("androidx.compose:compose-bom:2025.10.01"))
    testImplementation("androidx.compose.ui:ui-test-junit4")
    // Desktop JNA jar (with host jnidispatch) for the JVM-hosted Robolectric runtime.
    testImplementation("net.java.dev.jna:jna:$jnaVersion")

    androidTestImplementation("androidx.test:core:1.6.1")
    androidTestImplementation("androidx.test:runner:1.6.2")
    androidTestImplementation("androidx.test.ext:junit:1.2.1")
    androidTestImplementation(platform("androidx.compose:compose-bom:2025.10.01"))
    androidTestImplementation("androidx.compose.ui:ui-test-junit4")
    debugImplementation("androidx.compose.ui:ui-test-manifest")
    add("sideloadImplementation", project(":libadb"))
    if (peppyGoogleMessagesLink) {
        add("sideloadImplementation", files(googleMessagesAar))
    }
}

val checkGoogleMessagesAar = tasks.register("checkGoogleMessagesAar") {
    doLast {
        check(googleMessagesAar.asFile.isFile) {
            "peppyGoogleMessagesLink=true needs apps/android/googlemessages/build-aar.sh first"
        }
        verifyGoogleMessagesAarProvenance()
    }
}

if (peppyGoogleMessagesLink) {
    tasks.configureEach {
        if (name == "preSideloadBuild") {
            dependsOn(checkGoogleMessagesAar)
        }
    }
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

val googleMessagesDexPrefix = "Ldev/peppy/gmlink/"
val googleMessagesNativeLibrary = "libgojni.so"
val googleMessagesNotice = "assets/third_party/AGPL-3.0.txt"

fun googleMessagesLinkArtifacts(archives: List<File>, noticePath: String = googleMessagesNotice): Set<String> = buildSet {
    if (archives.any { archive ->
            ZipFile(archive).use { zip -> zip.entries().asSequence().any { it.name.matches(Regex("(?:base/)?lib/[^/]+/$googleMessagesNativeLibrary")) } }
        }) add(googleMessagesNativeLibrary)
    if (archiveDexContains(archives, googleMessagesDexPrefix)) add("dev/peppy/gmlink/")
    if (archiveContainsEntry(archives, noticePath)) add(googleMessagesNotice)
}

fun verifyGoogleMessagesLinkExclusion(archives: List<File>, variant: String, noticePath: String = googleMessagesNotice) {
    check(googleMessagesLinkArtifacts(archives, noticePath).isEmpty()) {
        "$variant archive must not contain Google Messages link artifacts: ${googleMessagesLinkArtifacts(archives, noticePath)}"
    }
}

fun verifySideloadGoogleMessagesLinkArtifacts(artifacts: Set<String>, enabled: Boolean) {
    val required = setOf(googleMessagesNativeLibrary, "dev/peppy/gmlink/", googleMessagesNotice)
    if (enabled) {
        check(artifacts == required) { "Sideload APK must contain all Google Messages link artifacts: missing ${required - artifacts}" }
    } else {
        check(artifacts.isEmpty()) { "Sideload APK must not contain Google Messages link artifacts: $artifacts" }
    }
}

fun writeArchiveFixture(archiveFile: File, entries: Map<String, ByteArray>) {
    ZipOutputStream(archiveFile.outputStream()).use { archive ->
        entries.forEach { (name, bytes) ->
            archive.putNextEntry(ZipEntry(name))
            archive.write(bytes)
            archive.closeEntry()
        }
    }
}

fun writeLittleEndian(bytes: ByteArray, offset: Int, value: Long, byteCount: Int) {
    repeat(byteCount) { index -> bytes[offset + index] = (value shr (index * 8)).toByte() }
}

fun elfFixture(loadAlignment: Long): ByteArray = ByteArray(120).also { bytes ->
    bytes[0] = 0x7f
    bytes[1] = 'E'.code.toByte()
    bytes[2] = 'L'.code.toByte()
    bytes[3] = 'F'.code.toByte()
    bytes[4] = 2
    bytes[5] = 1
    writeLittleEndian(bytes, 32, 64, 8)
    writeLittleEndian(bytes, 54, 56, 2)
    writeLittleEndian(bytes, 56, 1, 2)
    writeLittleEndian(bytes, 64, 1, 4)
    writeLittleEndian(bytes, 112, loadAlignment, 8)
}

fun writeStoredZipEntry(archive: ZipOutputStream, name: String, bytes: ByteArray) {
    val entry = ZipEntry(name).apply {
        method = ZipEntry.STORED
        size = bytes.size.toLong()
        compressedSize = bytes.size.toLong()
        crc = CRC32().apply { update(bytes) }.value
    }
    archive.putNextEntry(entry)
    archive.write(bytes)
    archive.closeEntry()
}

fun writeAlignedGoogleMessagesFixture(archiveFile: File, loadAlignment: Long, library: ByteArray = elfFixture(loadAlignment)) {
    val libraryPath = "lib/arm64-v8a/$googleMessagesNativeLibrary"
    val paddingSize = (0x4000 - (30 + "padding".length + 30 + libraryPath.length) % 0x4000) % 0x4000
    ZipOutputStream(archiveFile.outputStream()).use { archive ->
        writeStoredZipEntry(archive, "padding", ByteArray(paddingSize))
        writeStoredZipEntry(archive, libraryPath, library)
    }
}

fun littleEndian(bytes: ByteArray, offset: Int, byteCount: Int): Long {
    require(offset >= 0 && offset + byteCount <= bytes.size) { "Malformed ZIP or ELF data" }
    return (0 until byteCount).fold(0L) { value, index -> value or ((bytes[offset + index].toLong() and 0xff) shl (index * 8)) }
}

fun zipEntryDataOffsets(archiveFile: File): Map<String, Long> {
    val bytes = archiveFile.readBytes()
    val endOfCentralDirectory = (bytes.size - 22 downTo maxOf(0, bytes.size - 0xffff - 22)).firstOrNull { offset ->
        littleEndian(bytes, offset, 4) == 0x06054b50L
    } ?: throw GradleException("Malformed Google Messages link archive")
    fun safeOffset(value: Long): Int {
        check(value <= Int.MAX_VALUE.toLong()) { "Malformed Google Messages link archive" }
        return value.toInt()
    }
    var offset = safeOffset(littleEndian(bytes, endOfCentralDirectory + 16, 4))
    return buildMap {
        while (offset < bytes.size && littleEndian(bytes, offset, 4) == 0x02014b50L) {
            val nameLength = littleEndian(bytes, offset + 28, 2).toInt()
            val extraLength = littleEndian(bytes, offset + 30, 2).toInt()
            val commentLength = littleEndian(bytes, offset + 32, 2).toInt()
            val localHeaderOffset = safeOffset(littleEndian(bytes, offset + 42, 4))
            val name = bytes.copyOfRange(offset + 46, offset + 46 + nameLength).toString(Charsets.UTF_8)
            check(littleEndian(bytes, localHeaderOffset, 4) == 0x04034b50L) { "Malformed Google Messages link archive" }
            put(name, (localHeaderOffset + 30 + littleEndian(bytes, localHeaderOffset + 26, 2) + littleEndian(bytes, localHeaderOffset + 28, 2)))
            offset += 46 + nameLength + extraLength + commentLength
        }
    }
}

fun elfLoadAlignments(bytes: ByteArray): List<Long> {
    check(bytes.size >= 64 && bytes.copyOfRange(0, 4).contentEquals(byteArrayOf(0x7f, 'E'.code.toByte(), 'L'.code.toByte(), 'F'.code.toByte())))
    check(bytes[4].toInt() == 2 && bytes[5].toInt() == 1)
    val programOffset = littleEndian(bytes, 32, 8)
    val programEntrySize = littleEndian(bytes, 54, 2)
    val programCount = littleEndian(bytes, 56, 2)
    check(programOffset in 0..Int.MAX_VALUE.toLong() && programEntrySize >= 56)
    check(programOffset + programEntrySize * programCount <= bytes.size.toLong())
    return (0 until programCount.toInt()).mapNotNull { index ->
        val entryOffset = programOffset.toInt() + index * programEntrySize.toInt()
        if (littleEndian(bytes, entryOffset, 4) == 1L) littleEndian(bytes, entryOffset + 48, 8) else null
    }.also { alignments -> check(alignments.isNotEmpty()) { "Google Messages link native library has no LOAD segments" } }
}

fun verifyGoogleMessagesNativeLibraryAlignment(archives: List<File>) {
    try {
        archives.forEach { archive ->
            val offsets = if (archive.extension == "apk") zipEntryDataOffsets(archive) else emptyMap()
            ZipFile(archive).use { zip ->
                zip.entries().asSequence()
                    .filter { entry -> entry.name.matches(Regex("(?:base/)?lib/[^/]+/$googleMessagesNativeLibrary")) }
                    .forEach { entry ->
                        if (archive.extension == "apk") {
                            check(entry.method == ZipEntry.STORED && offsets.getValue(entry.name) % 0x4000 == 0L)
                        }
                        val alignments = zip.getInputStream(entry).use { it.readBytes() }.let(::elfLoadAlignments)
                        check(alignments.all { it >= 0x4000 })
                    }
            }
        }
    } catch (_: Exception) {
        throw GradleException("Google Messages link native library verification failed")
    }
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
    verifyGoogleMessagesLinkExclusion(archives, "Release AAB", "base/$googleMessagesNotice")
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

val verifySideloadGoogleMessagesLink = tasks.register("verifySideloadGoogleMessagesLink") {
    dependsOn("packageSideload")

    doLast {
        val metadata = layout.buildDirectory.file("outputs/apk/sideload/output-metadata.json").get().asFile
        val archives = apksFromMetadata(metadata, "sideload").toMutableList()
        if (gradle.taskGraph.allTasks.any { task -> task.name == "bundleSideload" }) {
            val sideloadAab = layout.buildDirectory.file("outputs/bundle/sideload/app-sideload.aab").get().asFile
            check(sideloadAab.isFile) { "Expected sideload AAB at $sideloadAab" }
            archives += sideloadAab
        }
        archives.forEach { archive ->
            val noticePath = if (archive.extension == "aab") "base/$googleMessagesNotice" else googleMessagesNotice
            verifySideloadGoogleMessagesLinkArtifacts(
                googleMessagesLinkArtifacts(listOf(archive), noticePath),
                peppyGoogleMessagesLink,
            )
            if (peppyGoogleMessagesLink) verifyGoogleMessagesNativeLibraryAlignment(listOf(archive))
        }
    }
}

tasks.configureEach {
    if (name == "bundleSideload") {
        finalizedBy(verifySideloadGoogleMessagesLink)
    }
}

val verifyReleaseRestrictedSetupExclusionFixtures = tasks.register("verifyReleaseRestrictedSetupExclusionFixtures") {
    doLast {
        check(runCatching { resolveGoogleMessagesLink("play", "true") }.isFailure) {
            "Play distribution must reject an explicit Google Messages link property"
        }
        check(!resolveGoogleMessagesLink("play", null)) { "Play distribution must disable the Google Messages link" }
        check(resolveGoogleMessagesLink("direct", null)) { "Direct distribution must enable the Google Messages link" }
        check(!resolveGoogleMessagesLink("direct", "false")) { "Explicit false must disable the Google Messages link" }

        val fixtureDirectory = layout.buildDirectory.dir("tmp/restricted-setup-exclusion-fixtures").get().asFile
        fixtureDirectory.deleteRecursively()
        fixtureDirectory.mkdirs()
        val provenanceFixtureDirectory = fixtureDirectory.resolve("google-messages-provenance")
        val provenanceFixtureGmlinkDirectory = provenanceFixtureDirectory.resolve("gmlink").apply { mkdirs() }
        provenanceFixtureGmlinkDirectory.resolve("fixture.go").writeText("package gmlink\n")
        provenanceFixtureDirectory.resolve("go.mod").writeText("go.mau.fi/mautrix-gmessages v0.0.0\n")
        provenanceFixtureDirectory.resolve("go.sum").writeText("fixture\n")
        val provenanceFixturePatch = provenanceFixtureDirectory.resolve("upstream-safety.patch")
        val provenanceFixtureChecksum = provenanceFixtureDirectory.resolve("upstream-safety.patch.sha256")
        val provenanceFixtureAar = provenanceFixtureDirectory.resolve("gmlink.aar").apply { writeText("old AAR") }
        val provenanceFixtureFile = provenanceFixtureDirectory.resolve("gmlink.aar.provenance")
        provenanceFixturePatch.writeText("original patch\n")
        provenanceFixtureChecksum.writeText("${sha256(provenanceFixturePatch)}  upstream-safety.patch\n")
        provenanceFixtureFile.writeText(
            googleMessagesProvenanceInputs(provenanceFixtureDirectory, provenanceFixtureAar)
                .entries.joinToString("\n") { (name, value) -> "$name=$value" } + "\n",
        )
        check(runCatching {
            verifyGoogleMessagesAarProvenance(
                provenanceFixtureDirectory,
                provenanceFixtureAar,
                provenanceFixtureFile,
            )
        }.isSuccess) { "Consistent patch, checksum pin and AAR provenance must pass verification" }
        provenanceFixturePatch.appendText("changed without checksum\n")
        val staleChecksumFailure = runCatching {
            googleMessagesProvenanceInputs(provenanceFixtureDirectory, provenanceFixtureAar)
        }.exceptionOrNull()
        check(staleChecksumFailure?.message?.contains("does not match upstream-safety.patch") == true) {
            "Changed safety patch must fail the checksum comparison, got: ${staleChecksumFailure?.message}"
        }
        provenanceFixtureChecksum.writeText("${sha256(provenanceFixturePatch)}  upstream-safety.patch\n")
        val staleProvenanceFailure = runCatching {
            verifyGoogleMessagesAarProvenance(
                provenanceFixtureDirectory,
                provenanceFixtureAar,
                provenanceFixtureFile,
            )
        }.exceptionOrNull()
        check(staleProvenanceFailure?.message?.contains("does not match current inputs") == true) {
            "Changed safety patch must reject stale AAR provenance, got: ${staleProvenanceFailure?.message}"
        }
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

        val googleMessagesArtifacts = mapOf(
            googleMessagesNativeLibrary to mapOf("lib/arm64-v8a/$googleMessagesNativeLibrary" to ByteArray(0)),
            "dev/peppy/gmlink/" to mapOf("classes.dex" to "${googleMessagesDexPrefix}Gmlink;".toByteArray()),
            googleMessagesNotice to mapOf(googleMessagesNotice to ByteArray(0)),
        )
        writeArchiveFixture(fixtureApk, mapOf("classes.dex" to ByteArray(0)))
        check(runCatching { verifyGoogleMessagesLinkExclusion(apksFromMetadata(metadata, "release"), "Release APK") }.isSuccess) {
            "Clean APK must pass the Google Messages link exclusion guard"
        }
        googleMessagesArtifacts.forEach { (artifact, entries) ->
            writeArchiveFixture(fixtureApk, entries)
            check(runCatching { verifyGoogleMessagesLinkExclusion(apksFromMetadata(metadata, "release"), "Release APK") }.isFailure) {
                "Synthetic $artifact APK artifact must fail the release exclusion guard"
            }
        }
        writeArchiveFixture(fixtureApk, mapOf("classes.dex" to "Ldev/peppy/mobile/googlemessages/GoogleMessagesLink;".toByteArray()))
        check(runCatching { verifyGoogleMessagesLinkExclusion(apksFromMetadata(metadata, "release"), "Release APK") }.isSuccess) {
            "Google Messages link stub seam must not fail the release exclusion guard"
        }
        val requiredGoogleMessagesArtifacts = googleMessagesArtifacts.keys
        requiredGoogleMessagesArtifacts.forEach { artifact ->
            check(runCatching {
                verifySideloadGoogleMessagesLinkArtifacts(requiredGoogleMessagesArtifacts - artifact, true)
            }.isFailure) { "Flag-on guard must reject a missing $artifact" }
        }
        requiredGoogleMessagesArtifacts.forEach { artifact ->
            check(runCatching {
                verifySideloadGoogleMessagesLinkArtifacts(setOf(artifact), false)
            }.isFailure) { "Flag-off guard must reject $artifact" }
        }

        val alignedGoogleMessagesFixture = fixtureDirectory.resolve("app-sideload-aligned.apk")
        writeAlignedGoogleMessagesFixture(alignedGoogleMessagesFixture, 0x4000)
        check(runCatching { verifyGoogleMessagesNativeLibraryAlignment(listOf(alignedGoogleMessagesFixture)) }.isSuccess) {
            "Synthetic 16 KiB-aligned Google Messages library must pass the sideload guard"
        }
        writeAlignedGoogleMessagesFixture(alignedGoogleMessagesFixture, 0x1000)
        check(runCatching { verifyGoogleMessagesNativeLibraryAlignment(listOf(alignedGoogleMessagesFixture)) }.isFailure) {
            "Synthetic 4 KiB-aligned Google Messages library must fail the sideload guard"
        }
        writeArchiveFixture(alignedGoogleMessagesFixture, mapOf("lib/arm64-v8a/$googleMessagesNativeLibrary" to elfFixture(0x4000)))
        check(runCatching { verifyGoogleMessagesNativeLibraryAlignment(listOf(alignedGoogleMessagesFixture)) }.isFailure) {
            "Synthetic ZIP-misaligned Google Messages library must fail the sideload guard"
        }
        val compressedGoogleMessagesAab = fixtureDirectory.resolve("app-sideload-compressed.aab")
        ZipOutputStream(compressedGoogleMessagesAab.outputStream()).use { archive ->
            archive.putNextEntry(ZipEntry("base/lib/arm64-v8a/$googleMessagesNativeLibrary"))
            archive.write(elfFixture(0x4000))
            archive.closeEntry()
        }
        check(runCatching { verifyGoogleMessagesNativeLibraryAlignment(listOf(compressedGoogleMessagesAab)) }.isSuccess) {
            "Compressed AAB Google Messages library with aligned ELF must pass the sideload guard"
        }
        listOf(
            ByteArray(3),
            elfFixture(0x4000).also { writeLittleEndian(it, 32, 4096, 8) },
            elfFixture(0x4000).also { writeLittleEndian(it, 56, 2, 2) },
            elfFixture(0x4000).also { writeLittleEndian(it, 32, Int.MAX_VALUE.toLong() + 1, 8) },
            elfFixture(0x4000).also { it[4] = 1 },
        ).forEach { malformedElf ->
            writeAlignedGoogleMessagesFixture(alignedGoogleMessagesFixture, 0x4000, malformedElf)
            val failure = runCatching { verifyGoogleMessagesNativeLibraryAlignment(listOf(alignedGoogleMessagesFixture)) }.exceptionOrNull()
            check(failure?.message == "Google Messages link native library verification failed") {
                "Malformed ELF must fail with the fixed verification message"
            }
        }
        alignedGoogleMessagesFixture.writeBytes(byteArrayOf(1, 2, 3))
        val malformedZipFailure = runCatching { verifyGoogleMessagesNativeLibraryAlignment(listOf(alignedGoogleMessagesFixture)) }.exceptionOrNull()
        check(malformedZipFailure?.message == "Google Messages link native library verification failed") {
            "Malformed ZIP must fail with the fixed verification message"
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

        googleMessagesArtifacts.forEach { (artifact, entries) ->
            writeArchiveFixture(fixtureAab, entries.mapKeys { (name, _) -> if (name == "classes.dex") "base/dex/classes.dex" else "base/$name" })
            check(runCatching { verifyGoogleMessagesLinkExclusion(listOf(fixtureAab), "Release AAB", "base/$googleMessagesNotice") }.isFailure) {
                "Synthetic $artifact AAB artifact must fail the release exclusion guard"
            }
        }

        val debugFixture = fixtureDirectory.resolve("app-debug.apk")
        writeArchiveFixture(debugFixture, googleMessagesArtifacts.getValue(googleMessagesNativeLibrary))
        check(runCatching { verifyGoogleMessagesLinkExclusion(listOf(debugFixture), "Debug APK") }.isFailure) {
            "Synthetic Google Messages link artifact must fail the debug exclusion guard"
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

verifyReleaseManifestSecurity.configure {
    dependsOn(verifyReleaseRestrictedSetupExclusionFixtures)
}

tasks.matching { it.name == "assembleRelease" }.configureEach {
    dependsOn(verifyReleaseRestrictedSetupExclusion)
    doLast {
        val metadata = layout.buildDirectory.file("outputs/apk/release/output-metadata.json").get().asFile
        verifyGoogleMessagesLinkExclusion(apksFromMetadata(metadata, "release"), "Release APK")
    }
}

tasks.matching { it.name == "bundleRelease" }.configureEach {
    dependsOn(verifyReleaseRestrictedSetupExclusion)
    doLast { verifyReleaseRestrictedSetupAabExclusion() }
}

tasks.matching { it.name == "assembleDebug" }.configureEach {
    dependsOn(verifyDebugRestrictedSetupExclusion)
    doLast {
        val metadata = layout.buildDirectory.file("outputs/apk/debug/output-metadata.json").get().asFile
        verifyGoogleMessagesLinkExclusion(apksFromMetadata(metadata, "debug"), "Debug APK")
    }
}

tasks.matching { it.name == "assembleSideload" }.configureEach {
    dependsOn(verifySideloadRestrictedSetupInclusion, verifySideloadManifestSecurity, verifySideloadGoogleMessagesLink)
}

gradle.projectsEvaluated {
    setOf("testSideloadUnitTest", "lintSideload", "assembleSideload").forEach { taskName ->
        check(tasks.names.contains(taskName)) { "Expected sideload task $taskName" }
    }
}

tasks.matching { it.name == "check" }.configureEach {
    dependsOn(verifyReleaseRestrictedSetupExclusionFixtures)
}
