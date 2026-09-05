// swift-tools-version: 6.2
import PackageDescription

let package = Package(
    name: "AmuxCore",
    platforms: [.iOS(.v26)],
    products: [
        .library(name: "AmuxCore", targets: ["AmuxCore"]),
    ],
    targets: [
        // Assembled by `wt run ios-rust` from the Rust bridge; the recipes that
        // build or test this package produce it first.
        .binaryTarget(name: "AmuxMobile", path: "../../../target/ios/AmuxMobile.xcframework"),
        .target(name: "AmuxCore", dependencies: ["AmuxMobile"]),
        .testTarget(name: "AmuxCoreTests", dependencies: ["AmuxCore"]),
    ],
    swiftLanguageModes: [.v6]
)
