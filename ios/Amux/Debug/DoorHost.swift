import AmuxCore
import AmuxDesign
import AmuxFeatures
import AmuxMobile
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
    /// Whether to draw as the system does for a reader who has asked for less
    /// motion, and for one who has asked for less transparency. Both are the
    /// device's own settings in a shipping build; here they are the state's,
    /// so a screen can be photographed with either on.
    private(set) var reduceMotion = false
    private(set) var reduceTransparency = false
    /// What the conversation on show has opened over itself, where the state
    /// asked for one the screen name does not imply.
    private(set) var overlay: ConversationOverlay?
    private(set) var stores = StoreBundle(account: AccountId("door"), clock: { Scenario.reading })
    /// The accounts a driven screen believes this phone has. Whether anything
    /// is reachable at all is an account fact, not a fleet fact, so the two
    /// gated home states need this as much as they need empty stores.
    private(set) var accounts = AccountRegistry()
    /// The sign-in a driven screen is in the middle of. Its own store rather
    /// than one of the account's: there is no account until it finishes.
    private(set) var signIn = SignInStore()
    /// Subscribing, as a driven screen has it: what is on offer, what is
    /// chosen and how a purchase went. Its own store because a subscription is
    /// bought before there is anything for an account's stores to hold.
    private(set) var paywall = PaywallStore()
    /// The account a driven screen is in the middle of giving up. Its own
    /// store for the same reason the app's is: the question outlives the page
    /// it is asked over, and a blocked deletion sends somebody out of the app
    /// and back again.
    private(set) var deletion = DeletionStore()
    /// The report a driven screen is in the middle of writing. Its own store
    /// for the same reason the app's is: what was frozen outlives the screen
    /// that froze it.
    private(set) var reports = ReportStore()

    /// What the App Store answers while the door is driving. The paywall is
    /// handed this rather than the real store, so no capture and no journey
    /// ever reaches StoreKit.
    let store = ScriptedStoreFront()
    /// The app's own accounts, when the door is driving the app rather than a
    /// fixture. A connection signs one in here, because that is where every
    /// screen reads whether this phone can reach anything.
    private var composed: AccountRegistry?

    /// What the screen on show has named, in the order it draws it. SwiftUI
    /// builds its accessibility tree only for an attached accessibility
    /// client, so a query from inside the process reads what the screen
    /// declared through `identified(_:)` rather than what VoiceOver would
    /// walk; the identifiers are the same ones, set by the same modifier.
    @ObservationIgnored var declared: [IdentifiedElement] = []

    /// What the cloud answers while the door is driving. Screens are handed
    /// this rather than the real service, so no capture ever reaches a network.
    let cloud = ScriptedCloudService()
    /// What comes back from the browser the hand-off opens, without opening
    /// one. The app's whole part in signing in is handing over a URL and being
    /// told what came back, and a driven launch is told the same thing — the
    /// system's own browser sheet is another process with somebody's password
    /// in it, and nothing outside it could answer it.
    let webAuth = ScriptedWebAuth()

    @ObservationIgnored private var bridge: BridgeClient?
    /// The relay this phone was told to reach, and the credential each account
    /// reaches it with, in the order they were signed in.
    @ObservationIgnored private var relayAddress: String?
    @ObservationIgnored private var credentials: [Credential] = []
    /// Which account's profile the runtime is reading. It follows the screen
    /// only once the runtime says it has switched.
    @ObservationIgnored private var runtimeAccount: AccountId?
    /// The last batch each account's connection produced, for playing one back
    /// as the late answer it would have been.
    @ObservationIgnored private var lastBatch: [AccountId: [Event]] = [:]

    struct Credential {
        let user: String
        let token: String
    }

    /// Every conversation this app has told the runtime to stop streaming, in
    /// the order it said so. A driver asking whether leaving a conversation
    /// reached the runtime reads this: the runtime's own account says which
    /// streams are open, and this says who asked for that.
    @ObservationIgnored private var unsubscribed: [AgentId] = []
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

    /// What has been recorded so far, for a report being frozen right now.
    ///
    /// Handed over as events rather than as the lines a bundle carries,
    /// because a freeze finishes the recording with the screen it is being
    /// taken on: somebody who has changed nothing since launch has recorded
    /// nothing, and a bundle whose trace is an empty file cannot tell a reader
    /// whether nothing was recorded or nothing happened.
    var traceEvents: [TraceEvent] { trace }

    func handle(_ request: DoorRequest) async -> DoorReply {
        switch request {
        case .open(let screen, let fixture): return open(screen: screen, fixture: fixture)
        case .cloud(let script):
            cloud.scripted = script.state
            return .ack
        case .store(let script):
            store.scripted = script.state
            return .ack
        case .calls:
            return .calls(cloud: cloud.calls.map(Self.said), store: store.calls.map(Self.said))
        case .accounts: return .accounts(accountsState())
        case .connect(let relay, let token, let user):
            return connect(relay: relay, token: token, user: user)
        case .addAccount(let user, let token):
            return addAccount(user: user, token: token)
        case .late(let account): return deliverLate(to: account)
        case .awaitReconciled(let seconds):
            return await awaitReconciled(within: seconds)
        case .awaitOffline(let seconds):
            return await awaitOffline(within: seconds)
        case .bridge: return .bridge(bridgeState())
        case .conversation(let agent):
            guard let identity = AgentId(agent), let conversation = stores.conversations[identity]
            else { return .error("no conversation is open with \(agent)") }
            return .conversation(ConversationReading(conversation))
        case .setModel(let agent, let name):
            guard let identity = AgentId(agent), let conversation = stores.conversations[identity],
                  let op = bridge?.dispatch(AgentWrite.model(name, of: identity))
            else { return .error("no conversation is open with \(agent)") }
            conversation.dispatched(op)
            return .ack
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
        case .states:
            return .states(Fixtures.drawn.map {
                DrawnState(screen: $0.screen.rawValue, state: $0.id, typeSize: $0.typeSize)
            })
        case .assist(let motion, let transparency):
            reduceMotion = motion
            reduceTransparency = transparency
            return .ack
        case .report(let path, let note, let marks):
            return report(to: path, note: note, marks: marks)
        case .screenshot:
            NotificationCenter.default.post(
                name: UIApplication.userDidTakeScreenshotNotification, object: nil)
            return .ack
        case .uploaded(let path): return uploaded(to: path)
        case .replay(let path): return replay(from: path)
        case .settle:
            await settle()
            return .ack
        case .query: return query()
        case .capture(let path): return await capture(to: path)
        case .tap(let identifier): return tap(identifier)
        case .type(let identifier, let text): return type(text, into: identifier)
        case .clear(let identifier): return clear(identifier)
        case .paste(let identifier, let text): return paste(text, into: identifier)
        case .move(let identifier, let from, let to):
            return move(from: from, to: to, in: identifier)
        case .attach(let agent, let kind, let name, let mime, let base64):
            return attach(to: agent, kind: kind, name: name, mime: mime, base64: base64)
        case .pair(let qr): return await pair(with: qr)
        case .pairByCode(let host, let pin): return await pair(with: pin, on: host)
        case .revoke(let host): return revoke(host)
        case .send(let agent, let text): return send(text, to: agent)
        case .sendDraft(let agent, let prose): return sendDraft(prose, to: agent)
        case .watch(let agent):
            guard let identity = AgentId(agent) else { return .error("no agent named \(agent)") }
            stores.openConversation(identity)
            return .ack
        case .awaitSendable(let agent, let seconds):
            return await awaitSendable(of: agent, within: seconds)
        case .requestChanges(let agent, let base): return requestChanges(of: agent, against: base)
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
    func adopt(_ stores: StoreBundle, accounts: AccountRegistry) {
        guard screen == nil else { return }
        self.stores = stores
        self.composed = accounts
    }

    private func open(screen name: String, fixture: String?) -> DoorReply {
        guard let screen = Screen(rawValue: name) else { return .error("no screen named \(name)") }
        let wanted = fixture ?? name
        // Whether this build draws the state is asked before the state is
        // looked up: a state nobody has written yet is unimplemented, and a
        // golden run over the whole manifest needs to hear that word rather
        // than a complaint about a missing fixture.
        guard Fixtures.isBuilt(screen, state: wanted) else {
            return .error("unimplemented: \(wanted)")
        }
        guard let fixture = Fixtures.named(wanted) else { return .error("no state named \(wanted)") }
        // A fresh bundle every time: a screen opened after another one must
        // not inherit the conversation the last one left behind — nor the
        // moment a fixture wound the clock back to.
        Scenario.reading = Scenario.now
        stores = StoreBundle(account: AccountId("door"), clock: { Scenario.reading })
        accounts = AccountRegistry()
        // Whole entries rather than one sign-in each: a declared state says
        // which accounts are signed out and what each has reached, and adding
        // them one at a time would sign every one of them in.
        accounts.restore(fixture.accounts)
        signIn = SignInStore(phase: fixture.signIn)
        store.scripted = fixture.store
        store.reset()
        paywall = PaywallStore(
            entitlement: fixture.accounts.first?.entitlement ?? .none,
            plans: fixture.store.plans, phase: fixture.paywall)
        deletion = DeletionStore(
            asking: fixture.deletion?.account, typed: fixture.deletion?.typed ?? "",
            phase: fixture.deletion?.phase ?? .asking)
        reports = Self.reporting(fixture.report)
        fixture.apply(stores)
        cloud.scripted = fixture.cloud
        cloud.reset()
        // A fixture that names no text size means the default one, not
        // whatever the last fixture left behind: one screen captured at an
        // accessibility size must not silently resize every screen after it.
        typeSize = fixture.typeSize.flatMap(DynamicTypeSize.init(doorName:)) ?? .large
        reduceMotion = fixture.reduceMotion
        reduceTransparency = fixture.reduceTransparency
        overlay = fixture.overlay
        show(screen)
        return .ack
    }

    /// The report a state declares, frozen on the fixture's own picture.
    ///
    /// The picture is committed beside the fixtures rather than photographed
    /// here: the door can only photograph the screen it is showing, and the
    /// screen it is showing is the report.
    private static func reporting(_ declared: Fixture.Reporting?) -> ReportStore {
        guard let declared, let capture = FrozenFixture.capture() else { return ReportStore() }
        return ReportStore(
            capture: capture,
            draft: ReportDraft(note: declared.note, marks: declared.marks),
            sending: {
                switch declared.sending {
                case .ready: .ready
                case .failed(let why): .failed(why)
                }
            }(),
            open: true)
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
        credentials = [Credential(user: user, token: token)]
        return start(relay: relay, active: user)
    }

    /// Signs another account in on this phone, with a relay credential of its
    /// own.
    ///
    /// The runtime is given its accounts when it starts, so a second one means
    /// starting it again with both. The account on screen does not change: a
    /// person who has just signed in somewhere else is still looking at what
    /// they were looking at, and moving them would be the app deciding for
    /// them.
    private func addAccount(user: String, token: String) -> DoorReply {
        guard let relay = relayAddress else { return .error("nothing has been connected") }
        credentials.removeAll { $0.user == user }
        credentials.append(Credential(user: user, token: token))
        let onScreen = composed?.selected?.value ?? runtimeAccount?.value ?? user
        return start(relay: relay, active: onScreen)
    }

    /// Starts the runtime for every account this phone has been given, reading
    /// the one named.
    private func start(relay: String, active user: String) -> DoorReply {
        guard let url = URL(string: relay), url.host != nil else {
            return .error("no relay at \(relay)")
        }
        guard credentials.contains(where: { $0.user == user }) else {
            return .error("no credential for \(user)")
        }
        stop()
        relayAddress = relay
        // A phone the driver handed a relay credential is a signed-in phone,
        // and the app's own account is where the screens read that from. The
        // one line a home is allowed above its rows says which of the things
        // that can be wrong is wrong; without this it would report a sign-in
        // that never failed instead of the connection that did, and a driver
        // reading the screen would be reading a state the harness invented.
        // An account somebody has already signed in on the screen is left
        // exactly as the screen left it: what it is entitled to came from the
        // account service, and a credential is not a second opinion about it.
        if let composed {
            for credential in credentials
            where !composed.accounts.contains(where: { $0.id == AccountId(credential.user) }) {
                composed.add(
                    SignedInAccount(
                        id: AccountId(credential.user),
                        email: "\(credential.user)@example.com",
                        displayName: credential.user),
                    entitlement: .active(source: .appStore, renews: nil))
            }
            // The account this connection is for is the account on screen:
            // a driver that connected as somebody else and left the screen on
            // the last one would be showing one account's name over another
            // account's machines. Selected before the seam below is installed,
            // so this is not mistaken for somebody pressing the switcher.
            composed.select(AccountId(user))
            // Signing in is what gives an account its own stores, so the ones
            // this connection fills are the ones the screens are now reading.
            if let signedIn = composed.stores { stores = signedIn }
            // Pressing an account in the switcher is what re-points the
            // runtime. The screen has no way to reach it, and this is where
            // the runtime is.
            composed.switching = { [weak self] id in self?.switched(to: id) }
        }
        runtimeAccount = AccountId(user)
        let directories = FileManager.default
        // One installation directory, holding a profile per account. Trust and
        // this phone's identity are an account's own — a machine admits a
        // device, and which device that is differs between the accounts on one
        // phone — and the profile is what keeps them apart, so one account's
        // pairings can never decide what the other one sees.
        let data = directories.temporaryDirectory
            .appendingPathComponent("door-data", isDirectory: true)
            .appendingPathComponent(user, isDirectory: true)
        let cache = directories.temporaryDirectory.appendingPathComponent("door-cache", isDirectory: true)
        try? directories.createDirectory(at: data, withIntermediateDirectories: true)
        try? directories.createDirectory(at: cache, withIntermediateDirectories: true)
        let configuration = BridgeConfiguration(
            dataDirectory: data, cacheDirectory: cache, deviceName: user,
            relay: BridgeConfiguration.Relay(
                url: relay,
                // A test relay on this machine has no certificate anybody
                // could trust, so a loopback URL is spoken to in the clear.
                tls: url.scheme == "https" ? .system : .plainLoopback),
            accounts: credentials.map {
                BridgeConfiguration.Account(id: $0.user, token: .fixed($0.token))
            },
            active: user,
            logPath: data.appendingPathComponent("door.log"))
        guard let client = try? BridgeClient(configuration: configuration) else {
            return .error("the runtime did not start")
        }
        bridge = client
        deviceName = user
        wire(stores, to: client)
        for agent in stores.conversations.keys { client.dispatch(.subscribe(agent: agent)) }
        pump = Task { @MainActor [weak self] in
            for await batch in client.events {
                self?.receive(batch)
            }
        }
        return .ack
    }

    /// Points one account's stores at the runtime.
    ///
    /// Called again for the account switched to, because its stores are new:
    /// the registry replaces them so that nothing of the account left behind
    /// can still be on screen, and a bundle nobody wired would draw a live
    /// account with no way to reach it.
    private func wire(_ stores: StoreBundle, to client: BridgeClient) {
        // Opening a conversation is what tells the runtime this client is
        // watching that agent. Without this the transcript for an agent
        // somebody opened would never be projected, and the screen would sit
        // empty beside a live connection.
        stores.watch = { [weak client] agent in client?.dispatch(.subscribe(agent: agent)) }
        stores.unwatch = { [weak client, weak self] agent in
            self?.unsubscribed.append(agent)
            client?.dispatch(.unsubscribe(agent: agent))
        }
        // What a screen decides reaches the machine through the same runtime
        // the feed arrives on. Answering an ask is a tap, so the connection
        // has to be reachable from the shell and not only from here.
        stores.dispatch = { [weak client] command in client?.dispatch(command) }
        // Picked bytes take their own path to the same connection: a
        // photograph is not JSON and does not belong in a command.
        stores.store = { [weak client] picked, bytes in client?.attach(picked, bytes: bytes) }
    }

    /// One batch from the runtime, credited to the account it answers for.
    ///
    /// That is the account the runtime is reading, which is not always the one
    /// on screen: a switch happens on the screen at once and in the runtime a
    /// moment later, and everything projected in between is about the account
    /// just left. The registry is what refuses those, so they go to it named
    /// as what they are rather than being applied to whatever is on screen.
    private func receive(_ batch: [Event]) {
        guard let composed, let answering = runtimeAccount else {
            stores.apply(batch)
            return
        }
        lastBatch[answering] = batch
        composed.deliver(batch, for: answering)
        for event in batch {
            guard case .opResult(let result) = event,
                case .selected(let account) = result.outcome
            else { continue }
            runtimeAccount = AccountId(account)
        }
        if let live = composed.stores, live !== stores {
            stores = live
            if let bridge { wire(live, to: bridge) }
        }
    }

    /// The account on screen changed. Tell the runtime, and read the stores
    /// the registry made for it.
    private func switched(to account: AccountId) {
        guard let bridge, let composed else { return }
        if let live = composed.stores, live !== stores {
            stores = live
            wire(live, to: bridge)
        }
        bridge.dispatch(.selectAccount(account.value))
    }

    /// Delivers again, under the name of the account it answered for, the last
    /// batch that account's connection produced.
    private func deliverLate(to account: String) -> DoorReply {
        guard let composed else { return .error("this app has no accounts of its own") }
        let id = AccountId(account)
        guard let batch = lastBatch[id] else {
            return .error("nothing has ever answered for \(account)")
        }
        composed.deliver(batch, for: id)
        return .accounts(accountsState())
    }

    /// The accounts this phone knows, as the registry the screens read has
    /// them.
    private func accountsState() -> AccountsState {
        let registry = composed ?? accounts
        return AccountsState(
            selected: registry.selected?.value,
            accounts: registry.accounts.map {
                AccountsState.Known(
                    id: $0.id.value, email: $0.account.email, signedIn: $0.signedIn,
                    entitlement: $0.entitlement.summary, hosts: $0.hosts,
                    attention: $0.attention)
            },
            dropped: registry.dropped)
    }

    /// One call a screen made of the scripted cloud, as a line.
    static func said(_ call: CloudCall) -> String {
        switch call {
        case .signIn: "signIn"
        case .account(let id): "account \(id)"
        case .entitlement(let id): "entitlement \(id)"
        case .connectToken(let id): "connectToken \(id)"
        case .recordPurchase(let id): "recordPurchase \(id)"
        case .requestDeletion(let id, let email): "requestDeletion \(id) as \(email)"
        case .uploadReport(let id, let parts):
            "uploadReport \(id) with \(parts.joined(separator: ", "))"
        }
    }

    /// One call the paywall made of the scripted App Store, as a line.
    static func said(_ call: StoreCall) -> String {
        switch call {
        case .plans: "plans"
        case .buy(let plan): "buy \(plan)"
        case .restore: "restore"
        case .finish(let transaction): "finish \(transaction)"
        case .unfinished: "unfinished"
        }
    }

    /// What the launch said after `-name`, or nothing where it did not say it.
    ///
    /// Read off the arguments themselves rather than through the defaults. The
    /// defaults parse an argument's value as a property list, so a pairing
    /// payload — which is JSON, and JSON braces are a plist dictionary — comes
    /// back as a dictionary and `string(forKey:)` answers nothing at all.
    /// Everything here is somebody else's text.
    static func said(_ name: String) -> String? {
        let arguments = ProcessInfo.processInfo.arguments
        guard let flag = arguments.firstIndex(of: "-\(name)"),
            arguments.index(after: flag) < arguments.endIndex
        else { return nil }
        return arguments[arguments.index(after: flag)]
    }

    /// A link the launch carried, for the app to open as if the system had
    /// handed it one. Nothing for an ordinary launch.
    static var linkAsLaunchAsks: URL? {
        said(Door.linkArgument).flatMap(URL.init(string:))
    }

    /// Connects and pairs as the launch itself asked, once.
    ///
    /// A driver speaking through the door connects by asking. A UI test cannot
    /// ask — it launches the app and presses things — so a launch it starts
    /// carries what to reach in its own arguments, and every tap after that
    /// happens against a real relay and a real machine rather than a fixture.
    /// A launch that says nothing about a relay is a launch of the app as
    /// itself and nothing here happens.
    func connectAsLaunchAsks() {
        guard bridge == nil,
            let relay = Self.said(Door.relayArgument),
            let token = Self.said(Door.tokenArgument),
            let user = Self.said(Door.userArgument)
        else { return }
        guard case .ack = connect(relay: relay, token: token, user: user) else {
            fatalError("the launch was told to connect to \(relay) and could not")
        }
        guard let payload = Self.said(Door.pairArgument) else { return }
        Task { @MainActor in
            if case .error(let complaint) = await pair(with: payload) {
                fatalError("the launch was told to pair and could not: \(complaint)")
            }
        }
    }

    /// Trusts a machine from the payload its pairing code carries, by the same
    /// two steps the pairing screen takes.
    ///
    /// Not a way past the protocol. The payload is authenticated against the
    /// machine that issued it, the machine answers with its own account of
    /// itself, and trust is written only by a second call naming that
    /// authenticated attempt — which is what makes the fingerprint a person
    /// reads a fingerprint the machine gave rather than one the payload
    /// claimed. What a driver skips here is the screen, not the handshake.
    private func pair(with qr: String) async -> DoorReply {
        guard bridge != nil else { return .error("nothing has been connected") }
        stores.pairing.open()
        guard stores.pair(link: qr) else {
            return .error("there was no runtime to authenticate the pairing with")
        }
        let authenticated = await pairingSettles(within: 60)
        guard case .confirming(let peer) = authenticated else {
            return .error("the machine did not authenticate the pairing: \(authenticated)")
        }
        guard stores.confirmPairing(peer) else {
            return .error("there was no runtime to write the trust with")
        }
        let written = await pairingSettles(within: 60)
        guard case .trusted(let host) = written else {
            return .error("the machine did not write the trust: \(written)")
        }
        return .paired(host: host)
    }

    /// Trusts a machine by the six-digit code it printed, through the store the
    /// pairing screen drives.
    ///
    /// The machine is found among the ones the relay is offering, because that
    /// is where a code's machine comes from: a code proves possession of one
    /// machine's offer and says nothing about which machine that is. From
    /// there it is the screen's own two calls — the digits go as they complete,
    /// and the trust is written against the attempt the machine answered with.
    private func pair(with pin: String, on host: String) async -> DoorReply {
        guard bridge != nil else { return .error("nothing has been connected") }
        guard let identity = HostId(host) else { return .error("no machine named \(host)") }
        guard let machine = await offered(identity, within: 60) else {
            return .error("the relay never offered \(host)")
        }
        stores.pairing.open(machine: machine)
        stores.pair(digits: pin)
        let authenticated = await pairingSettles(within: 60)
        guard case .confirming(let peer) = authenticated else {
            return .error("the machine did not authenticate the code: \(authenticated)")
        }
        guard stores.confirmPairing(peer) else {
            return .error("there was no runtime to write the trust with")
        }
        let written = await pairingSettles(within: 60)
        guard case .trusted(let name) = written else {
            return .error("the machine did not write the trust: \(written)")
        }
        return .paired(host: name)
    }

    /// Stops trusting one machine, by the call the paired devices sheet makes
    /// when its Revoke is pressed.
    private func revoke(_ host: String) -> DoorReply {
        guard bridge != nil else { return .error("nothing has been connected") }
        guard let identity = HostId(host) else { return .error("no machine named \(host)") }
        guard stores.revoke(identity) else {
            return .error("there was no runtime to withdraw the key with")
        }
        return .ack
    }

    /// Waits until the relay has offered this machine to pair with.
    private func offered(_ host: HostId, within seconds: Double) async -> HostEntry? {
        let deadline = Date().addingTimeInterval(seconds)
        while Date() < deadline {
            if let entry = stores.hosts.known(host) { return entry }
            await DoorFrames.next()
        }
        return nil
    }

    /// Waits until the pairing attempt is no longer with the machine.
    ///
    /// Polled a frame at a time, for the reason the other waits here are: the
    /// event stream is already being drained into the stores on this actor,
    /// and a second reader would take batches away from them.
    private func pairingSettles(within seconds: Double) async -> PairingStore.Phase {
        let deadline = Date().addingTimeInterval(seconds)
        while Date() < deadline {
            if stores.pairing.phase != .checking { return stores.pairing.phase }
            await DoorFrames.next()
        }
        return stores.pairing.phase
    }

    /// Waits until an agent's conversation will take a message.
    ///
    /// Polled a frame at a time, for the same reason the other waits here are:
    /// the event stream is already being drained into the stores on this
    /// actor, and a second reader would take batches away from them.
    private func awaitSendable(of agent: String, within seconds: Double) async -> DoorReply {
        guard bridge != nil else { return .error("nothing has been connected") }
        guard let identity = AgentId(agent) else { return .error("no agent named \(agent)") }
        let deadline = Date().addingTimeInterval(seconds)
        while Date() < deadline {
            if stores.conversations[identity]?.gate.accepts == true { return .ack }
            await DoorFrames.next()
        }
        let gate = stores.conversations[identity]?.gate
        return .error(
            "the conversation with \(agent) would still not take a message after \(seconds)s: "
            + "\(gate.map(String.init(describing:)) ?? "no conversation is open")")
    }

    /// Asks the host for an agent's changes. The host computes the diff and
    /// sends it back as an ordinary event, so nothing here waits for it: the
    /// chip appears when the answer lands, like every other fact on screen.
    private func requestChanges(of agent: String, against base: String) -> DoorReply {
        guard let bridge else { return .error("nothing has been connected") }
        guard let identity = AgentId(agent) else { return .error("no agent named \(agent)") }
        let against: JSONValue = base.isEmpty
            ? .object(["kind": .string("working_tree")])
            : .object(["kind": .string("branch"), "base": .string(base)])
        bridge.dispatch(.shared(.object([
            "command": .string("request_diff"),
            "agent": .string(identity.description),
            "base": against,
        ])))
        return .ack
    }

    /// Tries to send a message, through the gate the composer will send
    /// through.
    ///
    /// A refusal is an answer rather than a failure: the conversation states
    /// why it will not take a message, and the point of asking is to find out
    /// that nothing left the phone. So a refused send dispatches nothing at
    /// all — the host is never told, which is what the runner then confirms
    /// from the other side.
    private func send(_ text: String, to agent: String) -> DoorReply {
        guard let bridge else { return .error("nothing has been connected") }
        guard let identity = AgentId(agent) else { return .error("no agent named \(agent)") }
        guard let conversation = stores.conversations[identity] else {
            return .error("no conversation is open with \(agent)")
        }
        let subject = ConversationSubject(agent: identity, in: stores.fleet)
        guard conversation.gate.accepts else {
            let state = ConversationFootState(
                gate: conversation.gate, results: conversation.results, subject: subject)
            return .sendAttempt(
                delivered: false,
                reason: state?.detail ?? "This agent is not taking messages.")
        }
        let draft: [String: JSONValue] = [
            "command": .string("send"),
            "agent": .string(identity.description),
            "draft": .object([
                "segments": .array([.object([
                    "segment": .string("text"), "text": .string(text),
                ])]),
                "attachments": .array([]),
            ]),
        ]
        // The identifier the bridge answers with is what makes the host's
        // reply this conversation's rather than some other agent's.
        if let op = bridge.dispatch(.shared(.object(draft))) { conversation.dispatched(op) }
        return .sendAttempt(delivered: true, reason: nil)
    }

    /// Sends the draft a conversation is holding, with these words beside it.
    ///
    /// The message is built by the draft rather than here: a review element is
    /// spelled by the shared library and the attachment travels with it, and a
    /// second spelling of either in the driving tools would be a second thing
    /// to keep right. The gate is the one every message goes through, so a
    /// conversation that will not take one refuses this too.
    private func sendDraft(_ prose: String, to agent: String) -> DoorReply {
        guard bridge != nil else { return .error("nothing has been connected") }
        guard let identity = AgentId(agent) else { return .error("no agent named \(agent)") }
        guard let conversation = stores.conversations[identity] else {
            return .error("no conversation is open with \(agent)")
        }
        // Written at the caret, not over the whole draft: setting the body
        // drops every token whose stand-in is not in the new text, so a driver
        // that wrote its sentence that way would send a review or a photograph
        // it had just attached as an empty message.
        conversation.draft.insert(text: prose)
        let subject = ConversationSubject(agent: identity, in: stores.fleet)
        guard conversation.gate.accepts else {
            let state = ConversationFootState(
                gate: conversation.gate, results: conversation.results, subject: subject)
            return .sendAttempt(
                delivered: false,
                reason: state?.detail ?? "This agent is not taking messages.")
        }
        guard let command = conversation.draft.command(to: identity) else {
            return .error("the draft could not be turned into a message")
        }
        if let op = bridge?.dispatch(command) { conversation.dispatched(op) }
        conversation.draft.clear()
        return .sendAttempt(delivered: true, reason: nil)
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
            reconciliations: stores.fleet.reconciliations,
            hosts: stores.hosts.hosts.map(\.name).sorted(),
            agents: stores.fleet.rows.map(\.name).sorted(),
            relayAttempts: relay().attempts,
            relayRetries: relay().shortened,
            discovered: discovered(),
            watching: watching(),
            releasedStreams: unsubscribed.map(\.description).sorted())
    }

    /// The agents the runtime is holding a stream for, read off its own model
    /// rather than off the screens: a screen that has been left says nothing
    /// about whether the stream behind it was released.
    private func watching() -> [String] {
        guard let bridge, let json = bridge.snapshot(),
            let data = json.data(using: .utf8),
            let model = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
            let attached = model["attached"] as? [String: Any]
        else { return [] }
        return attached.keys.sorted()
    }

    /// What the runtime's link to the relay has done: every dial, and how many
    /// of those were early because somebody asked.
    ///
    /// A dial at a relay that is not there leaves no trace on the far side —
    /// nothing arrived to be counted — so this is where a driver reads it.
    /// Both are zero for a launch with no runtime behind it, which is every
    /// capture.
    private func relay() -> (attempts: UInt64, shortened: UInt64) {
        guard let bridge else { return (0, 0) }
        let json = bridge.withRuntime { handle -> String? in
            guard let owned = amux_mobile_relay_attempts(handle) else { return nil }
            defer { amux_mobile_free(owned) }
            return String(cString: owned)
        } ?? nil
        guard let json, let data = json.data(using: .utf8),
            let read = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
        else { return (0, 0) }
        return (read["attempts"] as? UInt64 ?? 0, read["shortened"] as? UInt64 ?? 0)
    }

    /// The machines the runtime has seen on the other side, this device
    /// excluded.
    ///
    /// Read from the shared model rather than from the stores, because the
    /// projected fleet deliberately carries only hosts this device is paired
    /// with, and a driver often wants to know what is out there before pairing
    /// with it. A machine here is proof the connection reached the relay and
    /// the relay reached a host.
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

    /// Writes the report this app would send into a directory the driver then
    /// reads out of the app's container.
    ///
    /// The screen is frozen here rather than taken from whatever the door has
    /// captured before: a report is about the frame somebody was looking at
    /// when they decided something was wrong, and freezing it through the same
    /// seam the screenshot path uses is what makes the bundle a real one.
    private func report(to path: String, note: String, marks: [ReportMark]) -> DoorReply {
        let directory = URL(fileURLWithPath: path, isDirectory: true)
        do {
            let parts = try DoorRecording.write(
                directory,
                freezer: ReportFreeze(),
                draft: ReportDraft(note: note, marks: marks),
                build: AppFiles.build,
                log: AppFiles.logTail)
            return .bundle(path: path, parts: parts)
        } catch {
            return .error("\(error)")
        }
    }

    /// Writes the last report the scripted account service was handed into a
    /// directory the driver then reads out of the app's container.
    ///
    /// The bundle is not rebuilt here. It is the one the Send button handed
    /// over, kept by the double at the boundary it crossed, so what a driver
    /// opens afterwards is what left the phone rather than a second assembly
    /// of the same capture — which is the only way `report.json`'s
    /// declarations can be read as a claim about the upload.
    private func uploaded(to path: String) -> DoorReply {
        guard let bundle = cloud.uploaded.last else {
            return .error("nothing has been uploaded")
        }
        let directory = URL(fileURLWithPath: path, isDirectory: true)
        do {
            try FileManager.default.createDirectory(
                at: directory, withIntermediateDirectories: true)
            var written: [String] = []
            for part in bundle.parts {
                guard let data = part.data else { continue }
                try data.write(to: directory.appendingPathComponent(part.name))
                written.append(part.name)
            }
            return .bundle(path: path, parts: written)
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
        let rebuilt = StoreBundle(account: AccountId("replay"), clock: { Scenario.now })
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
            // A route names a screen and nothing else, so the state it means
            // is that screen's own — the same rule opening one by name uses.
            guard Fixtures.isBuilt(screen, state: name) else {
                return .error("unimplemented: \(name)")
            }
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
            typeSize: typeSize.doorName,
            voiceOver: UIAccessibility.isVoiceOverRunning,
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
        if let input = writable(named: identifier, in: window) {
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

    /// Empties a named field, one character at a time through the field's own
    /// delete.
    ///
    /// Not by writing an empty string into it: a field's text is drawn from a
    /// draft the field itself keeps, and setting that from outside skips
    /// whatever the field does as characters leave — which for the composer is
    /// the whole of how a token comes apart. Deleting is what a finger does.
    private func clear(_ identifier: String) -> DoorReply {
        guard let window = DoorWindow.current else { return .error("no window on screen") }
        guard element(named: identifier, in: window) != nil else {
            return .error("no element named \(identifier)")
        }
        guard let input = writable(named: identifier, in: window) else {
            return .error("\(identifier) does not take text")
        }
        if !input.isFirstResponder { _ = input.becomeFirstResponder() }
        // Bounded, because a field that answers "still has text" after every
        // delete would otherwise hang the driver rather than fail it.
        var deletions = 0
        while input.hasText && deletions < 4096 {
            input.deleteBackward()
            deletions += 1
        }
        guard !input.hasText else { return .error("\(identifier) would not empty") }
        return .ack
    }

    /// Pastes text into a named field through the field's own paste.
    ///
    /// The clipboard is written first because that is where a paste reads
    /// from: what happens afterwards is the same `paste(_:)` the system's menu
    /// item sends, so a field that turns a long paste into a token turns this
    /// one into a token too.
    private func paste(_ text: String, into identifier: String) -> DoorReply {
        guard let window = DoorWindow.current else { return .error("no window on screen") }
        guard element(named: identifier, in: window) != nil else {
            return .error("no element named \(identifier)")
        }
        guard let input = writable(named: identifier, in: window) else {
            return .error("\(identifier) does not take a paste")
        }
        if !input.isFirstResponder { _ = input.becomeFirstResponder() }
        UIPasteboard.general.string = text
        input.paste(nil)
        return .ack
    }

    /// Moves one character of what a named field is holding.
    ///
    /// A token is one character in the sentence, so this is how a token is
    /// moved whole. The edit is the field's own — it goes through the same
    /// draft a dragged chip ends up written into — because the drag itself is
    /// the text view's private interaction and cannot be started from outside
    /// the process.
    private func move(from: Int, to: Int, in identifier: String) -> DoorReply {
        guard let window = DoorWindow.current else { return .error("no window on screen") }
        guard let field = writable(named: identifier, in: window) as? PastingTextView,
              let moved = field.moved
        else { return .error("\(identifier) holds nothing that can be moved") }
        moved(from, to)
        return .ack
    }

    /// Stores bytes for an agent as a picker's result, and answers once the
    /// runtime has taken them.
    ///
    /// Nothing here decides what a token says or where it lands: the store
    /// call is the one the pickers make, so the token appears at the caret
    /// when the host says the bytes are kept, and not before.
    private func attach(
        to agent: String, kind: String, name: String, mime: String, base64: String
    ) -> DoorReply {
        guard bridge != nil else { return .error("nothing has been connected") }
        guard let identity = AgentId(agent) else { return .error("no agent named \(agent)") }
        guard let bytes = Data(base64Encoded: base64), !bytes.isEmpty else {
            return .error("the attachment carried no bytes")
        }
        guard let picked = ArtifactKind(rawValue: kind), picked != .diff else {
            return .error("no attachment kind named \(kind)")
        }
        guard stores.attach(
            PickedAttachment(agent: identity, kind: picked, name: name, mime: mime),
            bytes: bytes)
        else { return .error("the attachment did not leave the phone") }
        return .ack
    }

    /// The field a driver means by a name.
    ///
    /// A name declared on a SwiftUI screen lands on an accessibility element
    /// and not on a view, so a field is often not reachable by its own name at
    /// all: the view under it is found by hit-testing where the screen said
    /// the name is, and failing that the field is whichever one already has
    /// the keyboard — which is where a keystroke or a paste would land anyway.
    ///
    /// Hit-testing misses more often than it looks like it should: SwiftUI
    /// draws a screen into a handful of views and does its own hit testing
    /// inside them, so what UIKit reports under a control's stated middle is
    /// usually a plain container with the field nowhere in it. When nothing
    /// holds the keyboard yet and the screen has exactly one text input, that
    /// input is unambiguously the field a name on that screen means.
    private func writable(
        named identifier: String, in window: UIWindow
    ) -> (any UIKeyInput & UIResponder)? {
        if let view = element(named: identifier, in: window) as? UIView,
           let input = DoorWindow.textInput(in: view) {
            return input
        }
        if let declared = declared.first(where: { $0.identifier == identifier }),
           let under = window.hitTest(
               CGPoint(x: declared.frame.midX, y: declared.frame.midY), with: nil),
           let input = DoorWindow.textInput(in: under) {
            return input
        }
        if let focused = DoorWindow.focused(in: window) { return focused }
        let inputs = DoorWindow.allTextViews(in: window)
        if inputs.count == 1, let sole = inputs.first as? (any UIKeyInput & UIResponder) {
            return sole
        }
        return nil
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
