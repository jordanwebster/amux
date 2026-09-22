import SnapshotTesting
import SwiftUI
import UIKit
import XCTest

@testable import Amux

@MainActor
final class ComponentSnapshotTests: XCTestCase {
    private let appearances: [(name: String, colorScheme: ColorScheme, interfaceStyle: UIUserInterfaceStyle)] = [
        ("light", .light, .light),
        ("dark", .dark, .dark),
    ]

    func testRetiredFullScreenSnapshotsHaveComponentCoverage() throws {
        let catalogIDs = ComponentCatalog.examples.map(\.id)
        XCTAssertFalse(catalogIDs.contains(where: \.isEmpty), "Component catalog IDs must not be empty")
        XCTAssertEqual(
            Set(catalogIDs).count,
            catalogIDs.count,
            "Component catalog IDs must be unique"
        )

        let manifest = try XCTUnwrap(
            Bundle(for: Self.self).url(forResource: "manifest", withExtension: "json"),
            "Goldens/manifest.json was not copied into the component snapshot test bundle"
        )
        let object = try JSONSerialization.jsonObject(with: Data(contentsOf: manifest))
        let root = try XCTUnwrap(object as? [String: Any])
        let screens = try XCTUnwrap(root["screens"] as? [[String: Any]])
        let available = Set(catalogIDs)
        for screen in screens {
            guard let replacements = screen["component_snapshots"] else { continue }
            let screenID = screen["id"] as? String ?? "<unnamed>"
            let ids = try XCTUnwrap(
                replacements as? [String],
                "\(screenID).component_snapshots must be an array of component IDs"
            )
            XCTAssertFalse(ids.isEmpty, "\(screenID).component_snapshots must not be empty")
            XCTAssertFalse(
                ids.contains(where: \.isEmpty),
                "\(screenID).component_snapshots contains an empty component ID"
            )
            let missing = Set(ids).subtracting(available)
            XCTAssertTrue(
                missing.isEmpty,
                "\(screenID).component_snapshots names absent catalog IDs: "
                    + missing.sorted().joined(separator: ", ")
            )
        }
    }

    func testComponents() async throws {
        UIView.setAnimationsEnabled(false)
        defer { UIView.setAnimationsEnabled(true) }
        let environment = ProcessInfo.processInfo.environment
        let requested = Self.requestedIDs(environment["AMUX_SNAPSHOT_ONLY"])
        let available = Set(ComponentCatalog.examples.map(\.id))
        let unknown = requested.subtracting(available)
        XCTAssertTrue(
            unknown.isEmpty,
            "Unknown component snapshot IDs: \(unknown.sorted().joined(separator: ", ")). "
                + "Available IDs: \(available.sorted().joined(separator: ", "))"
        )
        guard unknown.isEmpty else { return }

        let examples = ComponentCatalog.examples.filter { requested.isEmpty || requested.contains($0.id) }
        XCTAssertFalse(examples.isEmpty, "The component catalog contains no selected examples")

        let recording = environment["AMUX_RECORD_SNAPSHOTS"] == "1"
        let perturbing = environment["AMUX_SNAPSHOT_PERTURB"] == "1"
        print(
            "AMUX_SNAPSHOT_CONFIGURATION selected="
                + (requested.isEmpty ? "all" : requested.sorted().joined(separator: ","))
                + " record=\(recording ? 1 : 0) perturb=\(perturbing ? 1 : 0)"
                + " host=\(environment["AMUX_COMPONENT_SNAPSHOTS"] == "1" ? 1 : 0)"
        )
        let suiteStarted = ProcessInfo.processInfo.systemUptime
        if let raw = environment["AMUX_SNAPSHOT_RUNNER_STARTED"], let runnerStarted = Double(raw) {
            print(String(format: "AMUX_SNAPSHOT_TIMING startup=%.3fs", suiteStarted - runnerStarted))
        }

        for example in examples {
            for appearance in appearances {
                let started = ProcessInfo.processInfo.systemUptime
                await snapshot(
                    example: example,
                    appearance: appearance,
                    recording: recording,
                    perturbing: perturbing
                )
                print(String(
                    format: "AMUX_SNAPSHOT_TIMING component=%@.%@ seconds=%.3f",
                    example.id,
                    appearance.name,
                    ProcessInfo.processInfo.systemUptime - started
                ))
            }
        }
        print(String(
            format: "AMUX_SNAPSHOT_TIMING batch=%d seconds=%.3f",
            examples.count * appearances.count,
            ProcessInfo.processInfo.systemUptime - suiteStarted
        ))
    }

    private func snapshot(
        example: ComponentExample,
        appearance: (name: String, colorScheme: ColorScheme, interfaceStyle: UIUserInterfaceStyle),
        recording: Bool,
        perturbing: Bool
    ) async {
        let calendar: Calendar = {
            var value = Calendar(identifier: .gregorian)
            value.locale = Locale(identifier: "en_US_POSIX")
            value.timeZone = TimeZone(secondsFromGMT: 0)!
            return value
        }()
        let content = ComponentExampleView(example: example, appearance: appearance.colorScheme)
            .environment(\.locale, Locale(identifier: "en_US_POSIX"))
            .environment(\.calendar, calendar)
            .environment(\.timeZone, TimeZone(secondsFromGMT: 0)!)
            .environment(\.layoutDirection, .leftToRight)
            .transaction { $0.animation = nil }
        let rendered = AnyView(
            content.overlay(alignment: Alignment.leading) {
                if perturbing {
                    Rectangle().fill(Color(uiColor: .systemPink)).frame(width: 4)
                }
            }
        )
        let readiness = SnapshotReadiness()
        let controller = UIHostingController(rootView: ComponentReadinessReportingView(
            content: rendered,
            didChange: { readiness.elements = $0 }
        ))
        controller.overrideUserInterfaceStyle = appearance.interfaceStyle
        guard let windowScene = UIApplication.shared.connectedScenes
            .compactMap({ $0 as? UIWindowScene })
            .first
        else {
            XCTFail("The app-hosted snapshot test has no window scene")
            return
        }
        let previousKeyWindow = windowScene.windows.first(where: \.isKeyWindow)
        let window = UIWindow(windowScene: windowScene)
        window.frame = CGRect(origin: .zero, size: example.canvas)
        window.rootViewController = controller
        window.makeKeyAndVisible()
        defer {
            window.isHidden = true
            window.rootViewController = nil
            previousKeyWindow?.makeKey()
        }
        controller.view.frame = window.bounds
        controller.view.layoutIfNeeded()
        let becameReady = await waitUntilReady(
            identifier: example.readinessIdentifier,
            value: example.readinessValue,
            readiness: readiness,
            controller: controller
        )
        guard becameReady else {
            XCTFail(
                "Timed out waiting for \(example.id) to report "
                    + "\(example.readinessIdentifier ?? "<identifier>")="
                    + "\(example.readinessValue ?? "<value>"); observed "
                    + readiness.description
            )
            return
        }
        controller.view.setNeedsLayout()
        controller.view.layoutIfNeeded()

        let traits = UITraitCollection(traitsFrom: [
            UITraitCollection(displayScale: 3),
            UITraitCollection(displayGamut: .SRGB),
            UITraitCollection(layoutDirection: .leftToRight),
            UITraitCollection(userInterfaceStyle: appearance.interfaceStyle),
            UITraitCollection(accessibilityContrast: .normal),
        ])
        var strategy: Snapshotting<UIViewController, UIImage> = .image(
            drawHierarchyInKeyWindow: true,
            precision: 1,
            perceptualPrecision: 1,
            size: example.canvas,
            traits: traits
        )
        strategy.diffing = RoundingImageDiff.allowingChannelRounding(strategy.diffing)
        let failure = verifySnapshot(
            of: controller,
            as: strategy,
            named: "\(example.id).\(appearance.name)",
            record: recording ? .all : .never,
            file: #filePath,
            testName: "components"
        )
        if recording, failure?.contains("Record mode is on") == true {
            return
        }
        if let failure {
            XCTFail(failure)
        }
    }

    private func waitUntilReady(
        identifier: String?,
        value: String?,
        readiness: SnapshotReadiness,
        controller: UIViewController
    ) async -> Bool {
        guard let identifier, let value else { return true }
        let deadline = ProcessInfo.processInfo.systemUptime + 5
        repeat {
            controller.view.layoutIfNeeded()
            if readiness.contains(identifier: identifier, value: value) {
                return true
            }
            try? await Task.sleep(for: .milliseconds(10))
        } while ProcessInfo.processInfo.systemUptime < deadline
        return readiness.contains(identifier: identifier, value: value)
    }

    private static func requestedIDs(_ raw: String?) -> Set<String> {
        Set((raw ?? "").split(separator: ",").map(String.init).filter { !$0.isEmpty })
    }
}

@MainActor
private final class SnapshotReadiness {
    var elements: [(identifier: String, value: String?)] = []

    func contains(identifier: String, value: String) -> Bool {
        elements.contains { $0.identifier == identifier && $0.value == value }
    }

    var description: String {
        elements.map { "\($0.identifier)=\($0.value ?? "<nil>")" }.joined(separator: ", ")
    }
}
