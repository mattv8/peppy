import Foundation
import Testing
@testable import PeppyNative

@Suite struct CredentialTests {
    let token = String(repeating: "0f", count: 48)
    let fingerprint = String(repeating: "ab", count: 32)
    let vault = "6F0D2C8E-6E4B-4D7B-9E1A-0B7C2D4E6F80"
    let device = "1b2c3d4e-5f60-4718-8a9b-0c1d2e3f4a5b"

    func json(_ overrides: [String: Any] = [:], drop: String? = nil) -> Data {
        var object: [String: Any] = [
            "version": 1, "origin": "https://push.example.test", "vaultId": vault, "deviceId": device, "deviceToken": token,
        ]
        for (key, value) in overrides { object[key] = value }
        if let drop { object.removeValue(forKey: drop) }
        return try! JSONSerialization.data(withJSONObject: object)
    }

    @Test func parsesStrictV1AndCanonicalizesIdentity() throws {
        let credential = try DeviceCredential.parse(json(), allowLoopbackHTTP: false)
        #expect(credential.identity == EnrollmentIdentity(
            origin: "https://push.example.test", vaultId: vault.lowercased(), deviceId: device
        ))
        #expect(credential.token == token)
    }

    @Test func neverPrintsTheToken() throws {
        let credential = try DeviceCredential.parse(json(), allowLoopbackHTTP: false)
        var dumped = ""
        dump(credential, to: &dumped)
        for text in [String(describing: credential), String(reflecting: credential), dumped] {
            #expect(!text.contains(token))
        }
    }

    static let malformed: [String: (overrides: [String: String], drop: String?)] = [
        "extra field": (["extra": "x"], nil),
        "missing token": ([:], "deviceToken"),
        "version string": (["version": "1"], nil),
        "short token": (["deviceToken": "abc"], nil),
        "non-hex token": (["deviceToken": String(repeating: "zz", count: 48)], nil),
        "bad uuid": (["vaultId": "vault"], nil),
        "trailing slash": (["origin": "https://push.example.test/"], nil),
        "path": (["origin": "https://push.example.test/api"], nil),
        "uppercase host": (["origin": "https://Push.example.test"], nil),
        "default port": (["origin": "https://push.example.test:443"], nil),
        "userinfo": (["origin": "https://user@push.example.test"], nil),
        "query": (["origin": "https://push.example.test?x=1"], nil),
        "scheme": (["origin": "ftp://push.example.test"], nil),
    ]

    @Test(arguments: malformed.keys.sorted())
    func rejectsMalformedCredentials(_ name: String) {
        let (overrides, drop) = Self.malformed[name]!
        #expect(throws: ClientError.self, "\(name)") {
            try DeviceCredential.parse(json(overrides, drop: drop), allowLoopbackHTTP: true)
        }
    }

    @Test func rejectsNonNumericVersions() {
        #expect(throws: ClientError.invalidCredential("unsupported version")) {
            try DeviceCredential.parse(json(["version": true]), allowLoopbackHTTP: false)
        }
        #expect(throws: ClientError.invalidCredential("unsupported version")) {
            try DeviceCredential.parse(json(["version": 3]), allowLoopbackHTTP: false)
        }
    }

    @Test func parsesStrictV2AnchorAndKeepsV1Unanchored() throws {
        let anchored = try DeviceCredential.parse(
            json(["version": 2, "epochFloor": 2, "profileFingerprint": fingerprint]), allowLoopbackHTTP: false
        )
        #expect(anchored.anchor == EpochAnchor(epochFloor: 2, profileFingerprint: fingerprint))
        #expect(anchored.token == token)
        #expect(try DeviceCredential.parse(json(), allowLoopbackHTTP: false).anchor == nil)
    }

    @Test func rejectsMalformedV2Anchors() {
        let cases: [[String: Any]] = [
            ["version": 2],
            ["version": 2, "epochFloor": 2],
            ["version": 2, "profileFingerprint": fingerprint],
            ["version": 2, "epochFloor": 0, "profileFingerprint": fingerprint],
            ["version": 2, "epochFloor": "2", "profileFingerprint": fingerprint],
            ["version": 2, "epochFloor": 2.5, "profileFingerprint": fingerprint],
            ["version": 2, "epochFloor": -1, "profileFingerprint": fingerprint],
            ["version": 2, "epochFloor": 4_294_967_296, "profileFingerprint": fingerprint],
            ["version": 2, "epochFloor": true, "profileFingerprint": fingerprint],
            ["version": 2, "epochFloor": 2, "profileFingerprint": fingerprint.uppercased()],
            ["version": 2, "epochFloor": 2, "profileFingerprint": String(fingerprint.dropLast())],
            ["version": 1, "epochFloor": 2, "profileFingerprint": fingerprint],
            ["version": 2, "epochFloor": 2, "profileFingerprint": fingerprint, "extra": "x"],
        ]
        for object in cases {
            #expect(throws: ClientError.self, "\(object)") {
                try DeviceCredential.parse(json(object), allowLoopbackHTTP: false)
            }
        }
    }

    @Test func rejectsDuplicateV2Members() {
        let prefix = """
        {"version":2,"origin":"https://push.example.test","vaultId":"\(vault)","deviceId":"\(device)","deviceToken":"\(token)"
        """
        for members in [
            #","epochFloor":2,"epochFloor":1,"profileFingerprint":"\#(fingerprint)"}"#,
            #","epochFloor":2,"profileFingerprint":"\#(fingerprint)","profileFingerprint":"\#(String(repeating: "c", count: 64))"}"#,
            #","epochFloor":2,"epoch\u0046loor":1,"profileFingerprint":"\#(fingerprint)"}"#,
        ] {
            #expect(throws: ClientError.self) {
                try DeviceCredential.parse(Data((prefix + members).utf8), allowLoopbackHTTP: false)
            }
        }
    }

    @Test func rejectsOversizedFiles() {
        let padded = json() + Data(repeating: 0x20, count: DeviceCredential.maxFileBytes)
        #expect(throws: ClientError.invalidCredential("file too large")) { try DeviceCredential.parse(padded, allowLoopbackHTTP: false) }
    }

    @Test func plainHTTPOnlyForExplicitLoopback() throws {
        #expect(throws: ClientError.insecureOrigin) {
            try ServerOrigin(canonical: "http://push.example.test", allowLoopbackHTTP: true)
        }
        #expect(throws: ClientError.insecureOrigin) {
            try ServerOrigin(canonical: "http://127.0.0.1:8080", allowLoopbackHTTP: false)
        }
        let loopback = try ServerOrigin(canonical: "http://127.0.0.1:8080", allowLoopbackHTTP: true)
        #expect(loopback.url("/v1/vault").absoluteString == "http://127.0.0.1:8080/v1/vault")
        let https = try ServerOrigin(canonical: "https://push.example.test:8443", allowLoopbackHTTP: false)
        #expect(https.contains(URL(string: "https://PUSH.example.test:8443/v1/events")))
        #expect(!https.contains(URL(string: "https://push.example.test/v1/events")))
        #expect(!https.contains(URL(string: "https://evil.example.test:8443/v1/events")))
    }

    @Test func keychainItemsAreDeviceOnlyAfterFirstUnlockAndNeverSynchronized() {
        let store = KeychainSecureStore(service: "test.service")
        let query = store.query("database-key|x")
        let add = store.addAttributes(account: "database-key|x", data: Data("key".utf8))
        let update = store.updateAttributes(data: Data("replacement".utf8))
        #expect(query[kSecAttrSynchronizable as String] as? Bool == false)
        #expect(query[kSecAttrService as String] as? String == "test.service")
        #expect(query[kSecAttrAccount as String] as? String == "database-key|x")
        for attributes in [add, update] {
            #expect(attributes[kSecAttrAccessible as String] as? String == kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly as String)
            #expect(attributes[kSecAttrSynchronizable as String] as? Bool == false)
            #expect(attributes[kSecUseDataProtectionKeychain as String] as? Bool == true)
            #expect(attributes[kSecAttrAccessGroup as String] == nil)
        }
    }

    @Test func telephonyIsNeverExecutableInThisFoundation() {
        #expect(!TelephonyEligibility.evaluate().canExecuteCarrierCommands)
        let best = TelephonyEligibility.evaluate(osMajorVersion: 26, entitlementDeclared: true, executorImplemented: true)
        #expect(best.blockers == [.defaultAppAndRegionUnverified])
        #expect(!best.canExecuteCarrierCommands)
        let actual = TelephonyEligibility.evaluate(osMajorVersion: 18)
        #expect(actual.blockers == [
            .osTooOld(major: 18), .entitlementNotDeclared, .defaultAppAndRegionUnverified, .carrierExecutorNotImplemented,
        ])
    }
}
