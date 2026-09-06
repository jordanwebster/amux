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
    /// How many times an appearance has been asked for. The driven tree is
    /// keyed on this, so every request builds it afresh — including one that
    /// asks for the appearance already on show. Otherwise the first screen of
    /// a run is photographed as it was built at launch and every screen after
    /// it is photographed rebuilt, and the two do not draw glass identically.
    private(set) var appearances = 0
    /// The design every driven screen is drawn with. The app's own, unless a
    /// driver has asked for one token to be moved.
    private(set) var design: Design = .app
    private(set) var typeSize: DynamicTypeSize = .large
    private(set) var stores = StoreBundle(account: AccountId("door"), now: Scenario.now)
    /// The accounts a driven screen believes this phone has. Whether anything
    /// is reachable at all is an account fact, not a fleet fact, so the two
    /// gated home states need this as much as they need empty stores.
    private(set) var accounts = AccountRegistry()

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
    /// What this device called itself when it connected. The shared model
    /// lists this device alongside the ones it found, and a driver asking
    /// what is on the other side does not mean this one.
    @ObservationIgnored private var deviceName = ""

    /// What has been done to the view, in order, since the app started.
    ///
    /// The app has no navigation of its own yet, so everything here arrives
    /// through the door. When the screens land, they record their own routes,
    /// sheets and scroll positions into the same list and a report written by
    /// somebody using the app carries what they were looking at.
    @ObservationIgnored private var trace: [TraceEvent] = []

    func handle(_ request: DoorRequest) async -> DoorReply {
        switch request {
        case .open(let screen, let fixture): return open(screen: screen, fixture: fixture)
        case .cloud(let state):
            cloud.scripted = state
            return .ack
        case .connect(let relay, let token, let user):
            return connect(relay: relay, token: token, user: user)
        case .awaitReconciled(let seconds):
            return await awaitReconciled(within: seconds)
        case .awaitOffline(let seconds):
            return await awaitOffline(within: seconds)
        case .bridge: return .bridge(bridgeState())
        case .signposts: return .signposts(Signposts.marks)
        case .appearance(let appearance):
            await wear(appearance)
            trace.append(.appearance(appearance))
            return .ack
        case .perturb(let token):
            guard let token else {
                design = .app
                return .ack
            }
            guard let moved = Perturbation.design(.app, moving: token) else {
                return .error("the design has no colour token named \(token)")
            }
            design = moved
            return .ack
        case .dynamicType(let name):
            guard let size = DynamicTypeSize(doorName: name) else {
                return .error("no type size named \(name)")
            }
            typeSize = size
            trace.append(.dynamicType(name))
            return .ack
        case .report(let path): return report(to: path)
        case .replay(let path): return replay(from: path)
        case .settle:
            await settle()
            return .ack
        case .query: return query()
        case .capture(let path): return await capture(to: path)
        case .tap(let identifier): return tap(identifier)
        case .type(let identifier, let text): return type(text, into: identifier)
        // Answered here so the driver has an acknowledgement in hand before
        // the process goes; the server exits once the reply is written.
        case .shutdown: return .ack
        }
    }

    // MARK: - Driving

    /// Points the door at the stores the app is drawing for itself.
    ///
    /// A driver asking what is on screen must be answered about the screen
    /// that is on it. When nothing has been opened by name, that is the app's
    /// own home — filled from this phone's remembered fleet — so the door reads
    /// and connects to the same bundle rather than a spare one nobody can see.
    func adopt(_ stores: StoreBundle) {
        guard screen == nil else { return }
        self.stores = stores
    }

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
        accounts = AccountRegistry()
        for entry in fixture.accounts {
            accounts.add(entry.account, entitlement: entry.entitlement)
        }
        fixture.apply(stores)
        cloud.scripted = fixture.cloud
        cloud.reset()
        // A fixture that names no text size means the default one, not
        // whatever the last fixture left behind: one screen captured at an
        // accessibility size must not silently resize every screen after it.
        typeSize = fixture.typeSize.flatMap(DynamicTypeSize.init(doorName:)) ?? .large
        show(screen)
        return .ack
    }

    /// Puts the app into an appearance.
    ///
    /// The appearance is the window's interface style and nothing else: the
    /// design's colours are dynamic system colours and the glass is a system
    /// material, and both read the trait collection rather than SwiftUI's
    /// colour scheme.
    ///
    /// The screen is then built afresh, a frame later. Both halves matter. A
    /// material already on screen cross-fades to the new appearance over a
    /// length of time nobody publishes, so it is replaced rather than moved;
    /// and a replacement made in the same frame as the trait change is built
    /// while that change is still propagating, which is how a light screen
    /// ended up wearing the dark screen's plates.
    private func wear(_ appearance: Appearance) async {
        DoorWindow.current?.overrideUserInterfaceStyle =
            appearance == .dark ? .dark : .light
        self.appearance = appearance
        await DoorFrames.next()
        var immediately = Transaction()
        immediately.disablesAnimations = true
        withTransaction(immediately) { appearances += 1 }
    }

    /// Shows a screen without touching the stores. Opening a fixture replaces
    /// them; a replayed route must not, because the stores it is showing came
    /// out of the recording.
    private func show(_ screen: Screen) {
        self.screen = screen
        trace.append(.route(screen.rawValue))
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
        deviceName = user
        let stores = stores
        pump = Task { @MainActor in
            for await batch in client.events {
                stores.apply(batch)
            }
        }
        return .ack
    }

    /// Waits for the connection to have arrived somewhere: established, the
    /// fleet confirmed by the other side rather than remembered, and at least
    /// one machine seen there. All three, because a runtime that started and a
    /// runtime that reached a host are otherwise indistinguishable from here.
    ///
    /// Polling a frame at a time rather than awaiting the event stream,
    /// because the stream is already being drained into the stores on this
    /// actor; a second reader would take batches away from them.
    private func awaitReconciled(within seconds: Double) async -> DoorReply {
        guard bridge != nil else { return .error("nothing has been connected") }
        let deadline = Date().addingTimeInterval(seconds)
        while Date() < deadline {
            let state = bridgeState()
            if state.connection == "connected" && state.reconciled && !state.discovered.isEmpty {
                return .ack
            }
            await DoorFrames.next()
        }
        let state = bridgeState()
        return .error(
            "the connection did not arrive within \(seconds)s: \(state.connection), reconciled "
            + "\(state.reconciled), \(state.discovered.count) machines seen, "
            + "\(state.hosts.count) paired, \(state.agents.count) agents")
    }

    /// Waits for the connection to have given up: the relay is not answering
    /// and the app has said so to itself. Polled a frame at a time for the
    /// same reason as the wait above.
    private func awaitOffline(within seconds: Double) async -> DoorReply {
        guard bridge != nil else { return .error("nothing has been connected") }
        let deadline = Date().addingTimeInterval(seconds)
        while Date() < deadline {
            if stores.fleet.connection.state == .disconnected { return .ack }
            await DoorFrames.next()
        }
        return .error(
            "the connection was still \(stores.fleet.connection.state.rawValue) after \(seconds)s")
    }

    private func bridgeState() -> BridgeState {
        BridgeState(
            build: Bridge.build,
            started: bridge != nil,
            connection: stores.fleet.connection.state.rawValue,
            reconciled: stores.fleet.reconciled,
            hosts: stores.hosts.hosts.map(\.name).sorted(),
            agents: stores.fleet.rows.map(\.name).sorted(),
            discovered: discovered())
    }

    /// The machines the runtime has seen on the other side, this device
    /// excluded.
    ///
    /// Read from the shared model rather than from the stores, because the
    /// projected fleet deliberately carries only hosts this device is paired
    /// with — and this app cannot pair yet. A machine here is proof the
    /// connection reached the relay and the relay reached a host.
    private func discovered() -> [String] {
        guard let json = bridge?.snapshot(),
            let model = try? JSONSerialization.jsonObject(with: Data(json.utf8)) as? [String: Any],
            let rows = model["hosts"] as? [String: [String: Any]]
        else { return [] }
        return rows.values.compactMap { row -> String? in
            guard let entry = row["entry"] as? [String: Any],
                entry["online"] as? Bool == true,
                let name = entry["name"] as? String, name != deviceName
            else { return nil }
            return name
        }.sorted()
    }

    // MARK: - Recording and replaying

    /// Writes the shared runtime's recording and the view-state trace into a
    /// directory the driver then reads out of the app's container.
    private func report(to path: String) -> DoorReply {
        guard let bridge else {
            return .error("nothing is connected, so there is no recording to write")
        }
        let directory = URL(fileURLWithPath: path, isDirectory: true)
        do {
            let parts = try DoorRecording.write(directory, runtime: bridge, trace: trace)
            return .bundle(path: path, parts: parts)
        } catch {
            return .error("\(error)")
        }
    }

    /// Rebuilds the stores from a bundle and puts the screen back where its
    /// trace left it.
    ///
    /// Anything the app was connected to is stopped first: a replay is about
    /// a moment that already happened somewhere else, and a live connection
    /// delivering into the same stores would write over it.
    private func replay(from path: String) -> DoorReply {
        stop()
        let directory = URL(fileURLWithPath: path, isDirectory: true)
        let rebuilt = StoreBundle(account: AccountId("replay"), now: Scenario.now)
        let events: [Event]
        let recorded: [TraceEvent]
        do {
            events = try DoorRecording.replay(directory, into: rebuilt)
            recorded = try DoorRecording.trace(directory)
        } catch {
            return .error("\(error)")
        }
        stores = rebuilt
        screen = nil
        trace = []
        for event in recorded {
            if case .error(let why) = apply(event) { return .error(why) }
        }
        return .replayed(ReplayedState(
            events: events.count,
            agents: rebuilt.fleet.rows.map(\.name).sorted(),
            hosts: rebuilt.hosts.hosts.map(\.name).sorted(),
            entries: Dictionary(uniqueKeysWithValues: rebuilt.conversations.map {
                ($0.key.description, $0.value.entries.count)
            }),
            reconciled: rebuilt.fleet.reconciled,
            trace: recorded.count,
            screen: screen?.rawValue ?? "none"))
    }

    /// Puts one recorded view-state event back.
    ///
    /// A surface the app does not draw yet is a typed refusal rather than a
    /// silent skip, for the same reason opening an unbuilt screen is: a replay
    /// that quietly dropped the scroll position would come back looking right
    /// and be showing the wrong thing.
    private func apply(_ event: TraceEvent) -> DoorReply {
        switch event {
        case .route(let name):
            guard let screen = Screen(rawValue: name) else { return .error("no screen named \(name)") }
            guard DoorScreens.isBuilt(screen) else { return .error("unimplemented: \(name)") }
            show(screen)
            return .ack
        case .appearance(let appearance):
            Task { await wear(appearance) }
            trace.append(event)
            return .ack
        case .dynamicType(let name):
            guard let size = DynamicTypeSize(doorName: name) else {
                return .error("no type size named \(name)")
            }
            typeSize = size
            trace.append(event)
            return .ack
        // A sheet that was dismissed is nothing to put back, and the app has
        // no sheet to open and no transcript to scroll until the screens that
        // hold them are built.
        case .sheet(nil):
            trace.append(event)
            return .ack
        case .sheet(.some(let name)): return .error("unimplemented sheet: \(name)")
        case .scroll: return .error("unimplemented: scrolling a transcript")
        }
    }

    private func stop() {
        pump?.cancel()
        pump = nil
        bridge?.stop()
        bridge = nil
    }

    /// Waits for the screen to stop changing.
    ///
    /// Waits for the screen to stop changing.
    ///
    /// Three frames for a state change to land, be laid out and be drawn, and
    /// then the screen is drawn repeatedly until two passes agree. A fixed
    /// count of frames does not work: switching appearance re-resolves every
    /// system material on screen and UIKit takes a length of time over it that
    /// nobody publishes, so three frames photographed the half-way point and
    /// any larger number is a guess that is still too small on a loaded
    /// machine and wasted on an idle one.
    private func settle() async {
        for _ in 0..<3 { await DoorFrames.next() }
        guard let window = DoorWindow.current else { return }
        _ = await steady(window)
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

    /// Photographs the window, once it draws the same thing twice.
    ///
    /// Not a formality. Drawing the hierarchy into an image is what makes a
    /// system material resolve its backdrop for that renderer, and the first
    /// pass after an appearance change resolves it against the appearance
    /// before: a light screen came back wearing the dark screen's plates,
    /// every time and never the other way round. Two passes that agree are
    /// the only evidence available from inside the process that the picture
    /// is of the screen rather than of the one before it.
    ///
    /// The ceiling exists because some screens never stop — a row that is only
    /// remembered has a sweep passing over it forever — and a capture of one
    /// still has to happen. Whatever the last pass drew is what gets written.
    private func capture(to path: String) async -> DoorReply {
        guard let window = DoorWindow.current else { return .error("no window on screen") }
        guard let (image, data) = await steady(window) else {
            return .error("capture failed: the window would not draw")
        }
        return DoorCapture.write(image, data, to: path)
    }

    /// Draws the window until two passes agree, and hands back the last one.
    ///
    /// The ceiling exists because some screens never stop — a row that is only
    /// remembered has a sweep passing over it forever — and a capture of one
    /// still has to happen; whatever the last pass drew is what is returned.
    private func steady(_ window: UIWindow) async -> (UIImage, Data)? {
        var previous: Data?
        var last: (UIImage, Data)?
        for _ in 0..<30 {
            guard let image = DoorCapture.render(of: window),
                let data = image.pngData()
            else { return last }
            last = (image, data)
            if data == previous { return last }
            previous = data
            await DoorFrames.next()
        }
        return last
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
