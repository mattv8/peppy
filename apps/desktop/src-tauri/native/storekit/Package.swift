// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "StoreKitBridge",
    platforms: [.macOS(.v11)],
    products: [.library(name: "StoreKitBridge", type: .static, targets: ["StoreKitBridge"])],
    targets: [
        .target(
            name: "StoreKitBridge",
            dependencies: [],
            linkerSettings: [.linkedFramework("StoreKit"), .linkedFramework("AppKit")]
        ),
    ]
)
