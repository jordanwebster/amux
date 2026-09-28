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

    /// The golden manifest's components are exactly this catalogue, each
    /// with a sentence saying what it shows, so the index a reviewer reads
    /// is never missing a picture or naming one that is gone.
    func testTheManifestSaysWhatEveryComponentShows() throws {
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
        let components = try XCTUnwrap(root["components"] as? [[String: Any]])
        var described: [String] = []
        for component in components {
            let id = try XCTUnwrap(component["id"] as? String, "a component without an id")
            let shows = component["shows"] as? String ?? ""
            XCTAssertFalse(
                shows.trimmingCharacters(in: .whitespaces).isEmpty,
                "\(id) does not say what it shows"
            )
            described.append(id)
        }
        XCTAssertEqual(
            Set(described).subtracting(catalogIDs).sorted(), [],
            "The manifest describes components the catalogue no longer draws"
        )
        XCTAssertEqual(
            Set(catalogIDs).subtracting(described).sorted(), [],
            "The catalogue draws components the manifest does not describe"
        )
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
        // The component is drawn without the window's safe area (the status
        // bar) and photographed where it stands, on screen. SnapshotTesting's
        // own answer, moving the view far off screen just before drawing,
        // stops the render server finishing the glass: its shadow then
        // appears in some runs and not others.
        controller.safeAreaRegions = []
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
        // UIKit containers inside (a navigation stack) read UIKit's safe area,
        // not SwiftUI's regions, so the window's inset is cancelled there too.
        controller.additionalSafeAreaInsets = UIEdgeInsets(
            top: -window.safeAreaInsets.top, left: -window.safeAreaInsets.left,
            bottom: -window.safeAreaInsets.bottom, right: -window.safeAreaInsets.right)
        controller.view.frame = window.bounds
        controller.view.layoutIfNeeded()
        let deadline = ProcessInfo.processInfo.systemUptime + 5
        let becameReady = await waitUntilReady(
            identifier: example.readinessIdentifier,
            value: example.readinessValue,
            readiness: readiness,
            controller: controller,
            deadline: deadline
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
        guard let settled = await settledPhotograph(of: controller.view, traits: traits, deadline: deadline) else {
            XCTFail(
                "Timed out waiting for \(example.id).\(appearance.name) to settle: "
                    + "its photographs were still changing after 5 s"
            )
            return
        }
        var strategy: Snapshotting<UIViewController, UIImage> = .image(
            drawHierarchyInKeyWindow: true,
            precision: 1,
            perceptualPrecision: 1,
            size: example.canvas,
            traits: traits
        )
        strategy.diffing = RoundingImageDiff.allowingChannelRounding(strategy.diffing)
        strategy.snapshot = { _ in Async(value: settled) }
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
        controller: UIViewController,
        deadline: TimeInterval
    ) async -> Bool {
        guard let identifier, let value else { return true }
        repeat {
            controller.view.layoutIfNeeded()
            if readiness.contains(identifier: identifier, value: value) {
                return true
            }
            try? await Task.sleep(for: .milliseconds(10))
        } while ProcessInfo.processInfo.systemUptime < deadline
        return readiness.contains(identifier: identifier, value: value)
    }

    /// The picture compared is one the screen has settled on. Liquid Glass
    /// finishes appearing on the render server after SwiftUI has drawn it: its
    /// shadow fades in over most of a second, starting up to a third of a
    /// second after the view last changed, and holds each step for a few
    /// frames. A photograph taken on readiness, or two a frame apart that agree
    /// between steps, can catch it half drawn. The picture is taken once
    /// photographs every frame have stayed identical for a whole quiet window
    /// longer than any pause in that fade. The comparison is not loosened.
    private func settledPhotograph(
        of view: UIView,
        traits: UITraitCollection,
        deadline: TimeInterval
    ) async -> UIImage? {
        let quiet: TimeInterval = 0.5
        var settled = photograph(view, traits: traits)
        var since = ProcessInfo.processInfo.systemUptime
        repeat {
            await DisplayFrame.pass()
            let current = photograph(view, traits: traits)
            let now = ProcessInfo.processInfo.systemUptime
            if Self.pixels(of: current) != Self.pixels(of: settled) {
                settled = current
                since = now
            } else if now - since >= quiet {
                return settled
            }
        } while ProcessInfo.processInfo.systemUptime < deadline
        return nil
    }

    /// Draws the view as SnapshotTesting's key-window strategy does.
    private func photograph(_ view: UIView, traits: UITraitCollection) -> UIImage {
        view.layoutIfNeeded()
        Self.holdCarets(in: view)
        return UIGraphicsImageRenderer(bounds: view.bounds, format: .init(for: traits)).image { _ in
            view.drawHierarchy(in: view.bounds, afterScreenUpdates: true)
        }
    }

    /// A focused field's caret blinks for as long as it is shown, so a screen
    /// with one would never settle, or would settle on either phase. It is
    /// held lit instead.
    private static func holdCarets(in view: UIView) {
        for case let display as UITextSelectionDisplayInteraction in view.interactions {
            display.cursorView.isBlinking = false
        }
        view.subviews.forEach(holdCarets)
    }

    /// Both photographs come from the same renderer format, so their bitmaps
    /// share a layout and equal bytes mean equal pixels. (PNG encodings of
    /// equal pixels are not always byte-identical.)
    private static func pixels(of image: UIImage) -> Data? {
        image.cgImage?.dataProvider?.data as Data?
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

/// Waits for a whole display frame: the first display-link callback only marks
/// the next frame boundary, the second comes one full frame after it.
@MainActor
private final class DisplayFrame: NSObject {
    private var continuation: CheckedContinuation<Void, Never>?
    private var ticks = 0

    static func pass() async {
        let frame = DisplayFrame()
        await withCheckedContinuation { continuation in
            frame.continuation = continuation
            CADisplayLink(target: frame, selector: #selector(tick(_:))).add(to: .main, forMode: .common)
        }
    }

    @objc private func tick(_ link: CADisplayLink) {
        ticks += 1
        guard ticks == 2 else { return }
        link.invalidate()
        continuation?.resume()
        continuation = nil
    }
}
