import AmuxCore
import AmuxMobile
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
    /// What page the person was looking at, in the words the app names its
    /// pages by.
    private let route: () -> String?
    /// Where in the app that was. A replay puts the report back there, inside
    /// the shell, so this is the vocabulary that decides whether a bundle can
    /// be replayed to the place its picture was taken.
    private let place: () -> Place?
    /// Whose phone this was: the account on screen with whether its session is
    /// still good and what it may reach, or nothing when nobody was signed in.
    /// Never a credential.
    private let account: () -> AccountEntry?
    /// When the fleet on screen was last put in order, which is what every
    /// "11s ago" on it was measured from.
    private let ordered: () -> Date
    /// Every message half written on this phone and not sent. A draft is
    /// client-side and travels in nothing, so a report that did not carry one
    /// replays somebody's complaint with an empty composer under it.
    private let drafts: () -> [AgentId: MessageDraft]
    private let runtimeFailure: () -> String?

    init(
        window: @escaping () -> UIWindow? = { ReportFreeze.foreground },
        route: @escaping () -> String? = { DoorHost.shared.screen?.rawValue },
        place: @escaping () -> Place? = {
            DoorHost.shared.screen.map { Place.screen($0.rawValue) }
        },
        account: @escaping () -> AccountEntry? = { DoorHost.shared.accountOnScreen },
        ordered: @escaping () -> Date = { DoorHost.shared.stores.fleet.orderedAt },
        drafts: @escaping () -> [AgentId: MessageDraft] = {
            DoorHost.shared.stores.conversations.compactMapValues {
                $0.draft.isEmpty ? nil : $0.draft
            }
        },
        runtimeFailure: @escaping () -> String? = { nil }
    ) {
        self.window = window
        self.route = route
        self.place = place
        self.account = account
        self.ordered = ordered
        self.drafts = drafts
        self.runtimeFailure = runtimeFailure
    }

    func freeze() -> ReportCapture? {
        guard let window = window(), let frame = Self.photograph(window) else { return nil }
        var capture = ReportCapture(frame: frame, route: route(), runtimeFailure: runtimeFailure())
        switch Self.recording() {
        case .success(let json): capture.snapshot = json
        case .failure(let absent): capture.snapshotAbsent = absent.why
        }
        switch traceLines() {
        case .success(let lines): capture.trace = lines
        case .failure(let absent): capture.traceAbsent = absent.why
        }
        return capture
    }

    /// The view-state recording a bundle carries: what has been done to the
    /// view since launch, ending with the place the freeze happened in and the
    /// two facts that place was standing on — the clock and the account.
    ///
    /// The place on show is written even when nothing has changed the view.
    /// Somebody who opens the app and photographs the first thing they see has
    /// changed nothing, and a trace declared present but empty says "nothing
    /// was recorded" and "nothing happened" in the same breath — while a
    /// replay of it puts back no screen at all.
    ///
    /// What was open, what was half written and where each transcript was being
    /// read go after the place, because each of them is a state of a screen
    /// rather than a step on the way to one — and because putting the open card
    /// back needs the place it was open over to have been decided first. They
    /// are written in agent order so two freezes of the same screen produce the
    /// same file.
    ///
    /// The clock and the account go last because they are what the frozen
    /// screen was reading, not something that happened: a replay builds its
    /// stores from them before it folds a single message, and without them it
    /// rebuilds the right rows under the wrong name with the wrong ages on
    /// them.
    private func traceLines() -> Result<String, PartAbsent> {
        var events = DoorHost.shared.traceEvents
        if let place = place(), events.last != .route(place) {
            events.append(.route(place))
        }
        // Where the recording ends, read off the trail rather than asked for
        // again. A freeze asked for through the driving door is not told where
        // the app is — it is given no page to ask — and the trail is the one
        // account of it that is right either way.
        let ended: Place? = events.reversed().compactMap {
            if case .route(let place) = $0 { return place }
            return nil
        }.first
        if case .conversation(let agent)? = ended {
            events.append(.sheet(DoorHost.shared.panels[agent]))
        }
        let readings = DoorHost.shared.readings
        for agent in readings.keys.sorted(by: { $0.description < $1.description }) {
            events.append(.reading(agent, readings[agent]!))
        }
        let written = drafts()
        for agent in written.keys.sorted(by: { $0.description < $1.description }) {
            events.append(.draft(agent, written[agent]!))
        }
        events.append(.frozen(at: Date(), ordered: ordered()))
        events.append(.account(account()))
        do { return .success(try Trace.lines(events)) } catch {
            return .failure(PartAbsent("the view-state recording could not be written: \(error)"))
        }
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
