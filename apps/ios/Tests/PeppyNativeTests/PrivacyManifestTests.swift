import Foundation
import Testing

@Test func privacyManifestIsBundledAndDeclaresOnlyUsedRequiredReasonAPIs() throws {
    let iosDirectory = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent()
        .deletingLastPathComponent()
        .deletingLastPathComponent()
    let manifest = iosDirectory.appendingPathComponent("PeppyMobile/PrivacyInfo.xcprivacy")
    let project = try String(contentsOf: iosDirectory.appendingPathComponent("PeppyMobile.xcodeproj/project.pbxproj"))
    let data = try Data(contentsOf: manifest)
    let values = try #require(
        try PropertyListSerialization.propertyList(from: data, format: nil) as? [String: Any]
    )
    let accessed = try #require(values["NSPrivacyAccessedAPITypes"] as? [[String: Any]])
    let reasons = Dictionary(uniqueKeysWithValues: try accessed.map { entry in
        let category = try #require(entry["NSPrivacyAccessedAPIType"] as? String)
        let values = try #require(entry["NSPrivacyAccessedAPITypeReasons"] as? [String])
        return (category, Set(values))
    })

    #expect(values["NSPrivacyTracking"] as? Bool == false)
    #expect(values["NSPrivacyCollectedDataTypes"] == nil)
    #expect(reasons == [
        "NSPrivacyAccessedAPICategoryDiskSpace": ["E174.1"],
        "NSPrivacyAccessedAPICategoryFileTimestamp": ["C617.1"],
        "NSPrivacyAccessedAPICategorySystemBootTime": ["35F9.1"],
        "NSPrivacyAccessedAPICategoryUserDefaults": ["CA92.1"],
    ])
    #expect(project.contains("PrivacyInfo.xcprivacy in Resources"))
}
