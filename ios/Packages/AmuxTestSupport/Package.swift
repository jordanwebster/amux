// swift-tools-version: 6.2
import PackageDescription

let package = Package(
    name: "AmuxTestSupport",
    platforms: [.iOS(.v26)],
    products: [
        .library(name: "AmuxTestSupport", targets: ["AmuxTestSupport"]),
    ],
    dependencies: [
        .package(path: "../AmuxCore"),
        .package(path: "../AmuxDesign"),
        .package(path: "../AmuxFeatures"),
    ],
    targets: [
        .target(name: "AmuxTestSupport", dependencies: ["AmuxCore", "AmuxDesign", "AmuxFeatures"]),
        .testTarget(name: "AmuxTestSupportTests", dependencies: ["AmuxTestSupport"]),
    ],
    swiftLanguageModes: [.v6]
)
