import AmuxCore
import AmuxMobile
import AmuxTestSupport
import Foundation
import UIKit

/// Takes the picture and the two recordings a report is made of.
///
/// It exists only in a build with the driving tools compiled in, which is the
/// same rule the door and the recording writer follow: the calls it makes into
/// the shared library are behind the library's own debug-tools feature, and a
/// build a person installs has no report to write. `DebugTools.isCompiledIn`
/// is what the release scope audit asserts against, and this file is one of
/// the things it is asserting about.
///
/// Everything happens in one pass with no `await` in it. A freeze that yielded
/// between photographing the screen and freezing the recordings would produce
/// a bundle whose picture and whose messages were of different moments, which
/// is exactly the confusion a report is supposed to end.
@MainActor
final class ReportFreeze: ReportFreezing {
    /// The window to photograph, or nothing to find the one on screen. Handed
    /// in so a test can drive this against a window it built.
    private let window: () -> UIWindow?
    /// What the person was looking at, by the screen catalogue's name for it.
    private let route: () -> String?

    init(
        window: @escaping () -> UIWindow? = { ReportFreeze.foreground },
        route: @escaping () -> String? = { DoorHost.shared.screen?.rawValue }
    ) {
        self.window = window
        self.route = route
    }

    func freeze() -> ReportCapture? {
        guard let window = window(), let frame = Self.photograph(window) else { return nil }
        var capture = ReportCapture(frame: frame, route: route())
        switch Self.recording() {
        case .success(let json): capture.snapshot = json
        case .failure(let absent): capture.snapshotAbsent = absent.why
        }
        switch DoorHost.shared.traceLines {
        case .success(let lines): capture.trace = lines
        case .failure(let absent): capture.traceAbsent = absent.why
        }
        return capture
    }

    /// The window the person is looking at.
    static var foreground: UIWindow? {
        UIApplication.shared.connectedScenes
            .compactMap { $0 as? UIWindowScene }
            .first { $0.activationState == .foregroundActive || $0.activationState == .foregroundInactive }?
            .windows.first { $0.isKeyWindow }
    }

    /// The composited window, as PNG bytes with the size it was drawn at.
    ///
    /// `drawHierarchy` rather than rendering the layer tree: the layer tree
    /// draws none of the materials, and a report of a screen made of glass
    /// that came back as flat plates would be a picture of a different app.
    /// `afterScreenUpdates` is false on purpose — waiting for the next update
    /// would let whatever the person pressed land first, and the point of the
    /// flow is the frame before anything else happened.
    ///
    /// What this cannot include is the status bar and anything else the system
    /// draws outside the app's window. A report's picture is the app's own
    /// screen; the clock and the battery are not in it.
    private static func photograph(_ window: UIWindow) -> FrozenFrame? {
        let bounds = window.bounds
        guard bounds.width > 0, bounds.height > 0 else { return nil }
        let format = UIGraphicsImageRendererFormat()
        format.scale = window.screen.scale
        format.opaque = true
        let image = UIGraphicsImageRenderer(bounds: bounds, format: format).image { _ in
            window.drawHierarchy(in: bounds, afterScreenUpdates: false)
        }
        guard let png = image.pngData() else { return nil }
        return FrozenFrame(
            png: png, width: bounds.width, height: bounds.height,
            scale: Double(format.scale))
    }

    /// The shared runtime's own recording, frozen, or why there is none.
    private static func recording() -> Result<String, PartAbsent> {
        guard let running = BridgeClient.running else {
            return .failure(PartAbsent("nothing was connected, so there was no recording to freeze"))
        }
        let json = running.withRuntime { handle -> String? in
            guard let owned = amux_mobile_report_snapshot(handle) else { return nil }
            defer { amux_mobile_free(owned) }
            return String(cString: owned)
        }
        guard let json = json ?? nil else {
            return .failure(PartAbsent("the runtime did not answer with a recording"))
        }
        return .success(json)
    }
}
