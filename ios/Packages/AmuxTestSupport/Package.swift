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
        .target(
            name: "AmuxTestSupport",
            dependencies: ["AmuxCore", "AmuxDesign", "AmuxFeatures"],
            // The picture a report fixture is frozen on is not a package
            // resource. These sources are compiled straight into the debug app
            // rather than linked, so the picture is an app resource and this
            // target must not try to carry a second copy of it.
            exclude: ["Resources"]),
        .testTarget(name: "AmuxTestSupportTests", dependencies: ["AmuxTestSupport"]),
    ],
    swiftLanguageModes: [.v6]
)
