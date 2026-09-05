// swift-tools-version: 6.2
import PackageDescription

let package = Package(
    name: "AmuxFeatures",
    platforms: [.iOS(.v26)],
    products: [
        .library(name: "AmuxFeatures", targets: ["AmuxFeatures"]),
    ],
    dependencies: [
        .package(path: "../AmuxCore"),
        .package(path: "../AmuxDesign"),
    ],
    targets: [
        .target(name: "AmuxFeatures", dependencies: ["AmuxCore", "AmuxDesign"]),
    ],
    swiftLanguageModes: [.v6]
)
