// swift-tools-version: 6.2
import PackageDescription

let package = Package(
    name: "AmuxCore",
    platforms: [.iOS(.v26)],
    products: [
        .library(name: "AmuxCore", targets: ["AmuxCore"]),
        // The performance harness: workloads, budgets and the verdict. A
        // separate library because nothing a person installs measures itself.
        .library(name: "Instrumentation", targets: ["Instrumentation"]),
    ],
    targets: [
        // Assembled by `wt run ios-rust` from the Rust bridge; the recipes that
        // build or test this package produce it first.
        .binaryTarget(name: "AmuxMobile", path: "../../../target/ios/AmuxMobile.xcframework"),
        .target(name: "AmuxCore", dependencies: ["AmuxMobile"]),
        .target(name: "Instrumentation", dependencies: ["AmuxCore"]),
        .testTarget(
            name: "InstrumentationTests",
            dependencies: ["Instrumentation"],
            // The measurement document, read from where it is written, so a
            // budget changed in prose is a budget changed in the suite.
            resources: [.copy("Resources/IOS_PERFORMANCE.md")]
        ),
        .testTarget(
            name: "AmuxCoreTests",
            dependencies: ["AmuxCore"],
            // The pinned projection schema, read from the crate that defines
            // it, so a DTO change breaks this suite instead of drifting past
            // a stale copy.
            resources: [.copy("Resources/schema.json"), .copy("Resources/asks.json")]
        ),
    ],
    swiftLanguageModes: [.v6]
)
