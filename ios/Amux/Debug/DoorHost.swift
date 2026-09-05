import AmuxCore
import AmuxDesign
import AmuxFeatures
import Foundation
import Observation
import SwiftUI
import UIKit

/// The app's side of the driving door: the state a driver has put the app
/// into, and the one place a request is turned into an answer.
///
/// Everything a driver can change lives here rather than in the views, so a
/// screen under a golden run is the same view the app runs — it is only being
/// handed a different state.
@MainActor
@Observable
final class DoorHost {
    static let shared = DoorHost()

    /// The screen the door was asked to show, or nothing while the app is
    /// running as itself.
    private(set) var screen: Screen?
    private(set) var appearance: Appearance = .light
    private(set) var typeSize: DynamicTypeSize = .large
    private(set) var stores = StoreBundle(account: AccountId("door"), now: Scenario.now)

    /// What the screen on show has named, in the order it draws it. SwiftUI
    /// builds its accessibility tree only for an attached accessibility
    /// client, so a query from inside the process reads what the screen
    /// declared through `identified(_:)` rather than what VoiceOver would
    /// walk; the identifiers are the same ones, set by the same modifier.
    @ObservationIgnored var declared: [IdentifiedElement] = []

    /// What the cloud answers while the door is driving. Screens are handed
    /// this rather than the real service, so no capture ever reaches a network.
    let cloud = ScriptedCloudService()

    @ObservationIgnored private var bridge: BridgeClient?
    @ObservationIgnored private var pump: Task<Void, Never>?

    func handle(_ request: DoorRequest) async -> DoorReply {
        switch request {
        case .open(let screen, let fixture): return open(screen: screen, fixture: fixture)
        case .cloud(let state):
            cloud.scripted = state
            return .ack
        case .connect(let relay, let token, let user):
            return connect(relay: relay, token: token, user: user)
        case .appearance(let appearance):
            self.appearance = appearance
            return .ack
        case .dynamicType(let name):
            guard let size = DynamicTypeSize(doorName: name) else {
                return .error("no type size named \(name)")
            }
            typeSize = size
            return .ack
        case .settle:
            await settle()
            return .ack
        case .query: return query()
        case .capture(let path): return capture(to: path)
        case .tap(let identifier): return tap(identifier)
        case .type(let identifier, let text): return type(text, into: identifier)
        // Answered here so the driver has an acknowledgement in hand before
        // the process goes; the server exits once the reply is written.
        case .shutdown: return .ack
        }
    }

    // MARK: - Driving

    private func open(screen name: String, fixture: String?) -> DoorReply {
        guard let screen = Screen(rawValue: name) else { return .error("no screen named \(name)") }
        // Whether the screen exists is asked first: a screen nobody has built
        // is unimplemented whether or not its state has been written yet, and
        // a golden run over the whole manifest needs to hear that word rather
        // than a complaint about a fixture.
        guard DoorScreens.isBuilt(screen) else { return .error("unimplemented: \(name)") }
        let wanted = fixture ?? name
        guard let fixture = Fixtures.named(wanted) else { return .error("no state named \(wanted)") }
        // A fresh bundle every time: a screen opened after another one must
        // not inherit the conversation the last one left behind.
        stores = StoreBundle(account: AccountId("door"), now: Scenario.now)
        fixture.apply(stores)
        cloud.scripted = fixture.cloud
        cloud.reset()
        if let named = fixture.typeSize, let size = DynamicTypeSize(doorName: named) {
            typeSize = size
        }
        self.screen = screen
        return .ack
    }

    private func connect(relay: String, token: String, user: String) -> DoorReply {
        guard let url = URL(string: relay), url.host != nil else {
            return .error("no relay at \(relay)")
        }
        stop()
        let directories = FileManager.default
        let data = directories.temporaryDirectory.appendingPathComponent("door-data", isDirectory: true)
        let cache = directories.temporaryDirectory.appendingPathComponent("door-cache", isDirectory: true)
        try? directories.createDirectory(at: data, withIntermediateDirectories: true)
        try? directories.createDirectory(at: cache, withIntermediateDirectories: true)
        let configuration = BridgeConfiguration(
            dataDirectory: data, cacheDirectory: cache, deviceName: user,
            relay: BridgeConfiguration.Relay(
                url: relay,
                // A test relay on this machine has no certificate anybody
                // could trust, so a loopback URL is spoken to in the clear.
                tls: url.scheme == "https" ? .system : .plainLoopback,
                token: .fixed(token)),
            logPath: data.appendingPathComponent("door.log"))
        guard let client = try? BridgeClient(configuration: configuration) else {
            return .error("the runtime did not start")
        }
        bridge = client
        let stores = stores
        pump = Task { @MainActor in
            for await batch in client.events {
                stores.apply(batch)
            }
        }
        return .ack
    }

    private func stop() {
        pump?.cancel()
        pump = nil
        bridge?.stop()
        bridge = nil
    }

    /// Waits for the screen to stop changing. Three frames, because a state
    /// change lands one frame, is laid out the next and is drawn the third.
    private func settle() async {
        for _ in 0..<3 { await DoorFrames.next() }
    }

    // MARK: - Reading and driving what is drawn

    private func query() -> DoorReply {
        guard let window = DoorWindow.current else { return .error("no window on screen") }
        let declared = declared.map {
            VisibleElement(
                identifier: $0.identifier, label: $0.label, value: $0.value,
                frame: VisibleFrame(
                    x: $0.frame.origin.x, y: $0.frame.origin.y,
                    width: $0.frame.width, height: $0.frame.height),
                enabled: $0.enabled)
        }
        // The UIKit leaves the app registers are real views and do appear in
        // the accessibility tree, so both sources are read and neither screen
        // kind is invisible to a journey.
        let named = Set(declared.map(\.identifier))
        let leaves = VisibleTree.elements(of: window).filter { !named.contains($0.identifier) }
        return .state(VisibleState(
            screen: screen?.rawValue ?? "none",
            elements: declared + leaves,
            reconciled: stores.fleet.reconciled,
            shimmering: stores.fleet.rows.filter { !$0.confirmed }.count))
    }

    private func capture(to path: String) -> DoorReply {
        guard let window = DoorWindow.current else { return .error("no window on screen") }
        return DoorCapture.png(of: window, to: path)
    }

    private func tap(_ identifier: String) -> DoorReply {
        guard let window = DoorWindow.current else { return .error("no window on screen") }
        guard let element = element(named: identifier, in: window) else {
            return .error("no element named \(identifier)")
        }
        // What VoiceOver does to a control, which is the one way to act on a
        // SwiftUI element from inside the process.
        guard element.accessibilityActivate() else {
            return .error("\(identifier) did not activate")
        }
        return .ack
    }

    private func type(_ text: String, into identifier: String) -> DoorReply {
        guard let window = DoorWindow.current else { return .error("no window on screen") }
        guard let element = element(named: identifier, in: window) else {
            return .error("no element named \(identifier)")
        }
        if let field = element as? UIView, let input = DoorWindow.textInput(in: field) {
            if !input.isFirstResponder { _ = input.becomeFirstResponder() }
            input.insertText(text)
            return .ack
        }
        guard let input = element as? UIKeyInput else {
            return .error("\(identifier) does not take text")
        }
        input.insertText(text)
        return .ack
    }

    /// The object behind a name: the accessibility tree first, and otherwise
    /// whatever the screen declared, found by hit-testing where it said it
    /// was. A SwiftUI control is not a view of its own, so the second route
    /// reaches the hosting view that draws it and asks that to act.
    private func element(named identifier: String, in window: UIWindow) -> NSObject? {
        if let found = VisibleTree.find(identifier, in: window) { return found }
        guard let declared = declared.first(where: { $0.identifier == identifier }) else {
            return nil
        }
        return window.hitTest(CGPoint(x: declared.frame.midX, y: declared.frame.midY), with: nil)
    }
}
