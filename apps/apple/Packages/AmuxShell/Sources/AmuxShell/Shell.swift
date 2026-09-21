import AmuxCore
import AmuxDesign
import AmuxFeatures
import PhotosUI
import SwiftUI
import UniformTypeIdentifiers
#if canImport(UIKit)
import UIKit
#endif

/// Puts text on the system clipboard.
///
/// The clipboard is the system's and not this app's, so it is reached from
/// here rather than from a screen: a screen that touched UIKit would be a
/// screen that could not be photographed or replayed away from a device.
@MainActor
private func copy(_ text: String) {
    #if canImport(UIKit)
    UIPasteboard.general.string = text
    #endif
}

/// What the shell cannot do for itself and hands back to whoever assembled the
/// app: anything that reaches the cloud, the store or another account.
public enum ShellAction: Equatable, Sendable {
    case selectAccount(AccountId)
    case addAccount
    /// Somebody asked to sign in. Where that leads is a page; what it does
    /// when it gets there is the cloud's, which is why both leave the shell.
    case signIn
    /// Sign back into one account this phone already lists. Kept apart from
    /// adding one, because amux.sh is asked for that account by name and
    /// whoever comes back is checked against it.
    case signInAgain(AccountId)
    /// A sign-in came back as another account than the one asked for, and
    /// the person wants it on the phone anyway.
    case keepSignIn
    /// The same, and the person does not.
    case discardSignIn
    /// The press on the sign-in screen itself: hand off to the account
    /// service. The shell does not do this because it reaches nothing.
    case handOffSignIn
    case subscribe
    /// Buy the plan the paywall has selected.
    case buySubscription
    /// Put back a subscription this Apple Account already has.
    case restorePurchases
    /// Send a purchase amux.sh has not confirmed to it again.
    case retryPurchase
    /// Leave an account. It stays listed with Sign In beside it.
    case signOutAccount(AccountId)
    /// Somebody asked to take an account off this phone. What that does is
    /// said first, over the page it was asked from.
    case removeAccount(AccountId)
    /// The answer to that question.
    case confirmRemoval
    case cancelRemoval
    /// Somebody asked to give up an account for good. What that costs is a
    /// question the app asks before anything leaves this phone.
    case deleteAccount(AccountId)
    /// The answer to that question, with the address typed. It reaches the
    /// account service, so the shell does not do it.
    case confirmDeletion
    /// Changed their mind about it.
    case cancelDeletion
    /// Light, dark, or whatever the phone is set to.
    case wear(Appearance?)

}

/// Leaves the app for a page somewhere else: a billing portal, the App Store's
/// own subscriptions page, a way to reach a person.
///
/// Reached from here rather than from a screen for the same reason the
/// clipboard is: a screen that opened a URL would be a screen that could not
/// be photographed or replayed away from a device.
@MainActor
private func leave(for url: URL) {
    #if canImport(UIKit)
    UIApplication.shared.open(url)
    #endif
}

/// The app: three tabs, a stack under each, and a title menu on the Agents
/// tab that names the account whose fleet is on show.
///
/// Everything here is navigation and composition. The screens themselves are
/// functions of their stores and never reach for a route, which is why one can
/// be captured or replayed on its own.
public struct Shell: View {
    @Environment(\.design) private var design
    private let router: Router
    private let accounts: AccountRegistry
    private let stores: StoreBundle
    /// The one sign-in this phone has in flight. It is not an account's store
    /// because there is no account until it finishes.
    private let signIn: SignInStore
    /// What is on offer and how a purchase went. One per app, not per account:
    /// the App Store sells to an Apple Account, not to an amux one.
    private let paywall: PaywallStore
    /// The account this phone is in the middle of giving up, if any. Its own
    /// store because the question outlives the page it is asked over: leaving
    /// to cancel a renewal and coming back finds the same question, with the
    /// address still typed.
    private let deletion: DeletionStore
    /// The account this phone is asking about taking off it, if any.
    private let removal: RemovalStore
    /// What the app is wearing, or nothing for whatever the phone is set to.
    private let appearance: Appearance?
    /// Freezes the screen and opens a report on it, from Help. The app owns
    /// the capture; the shell only knows where the row that asks for it is.
    private let report: @MainActor () -> Void
    /// Where a build with the reporting tools in it keeps what a conversation
    /// has open and where it is being read, and where a replay puts them back.
    /// Nothing in the shipping app supplies one.
    private let recording: ConversationRecording?
    private let actions: @MainActor (ShellAction) -> Void

    public init(
        router: Router,
        accounts: AccountRegistry,
        stores: StoreBundle,
        signIn: SignInStore,
        paywall: PaywallStore,
        deletion: DeletionStore,
        removal: RemovalStore = RemovalStore(),
        appearance: Appearance? = nil,
        report: @escaping @MainActor () -> Void = {},
        recording: ConversationRecording? = nil,
        actions: @escaping @MainActor (ShellAction) -> Void
    ) {
        self.appearance = appearance
        self.deletion = deletion
        self.removal = removal
        self.router = router
        self.accounts = accounts
        self.stores = stores
        self.signIn = signIn
        self.paywall = paywall
        self.report = report
        self.recording = recording
        self.actions = actions
    }

    public var body: some View {
        @Bindable var router = router
        ZStack(alignment: .bottom) {
            ZStack {
                NavigationStack(path: $router.agentsPath) {
                    AgentsTab(
                        router: self.router, accounts: accounts, stores: stores,
                        actions: actions)
                        .navigationDestination(for: Route.self) { page($0) }
                }
                .tabSurface(selected: router.tab == .agents)

                NavigationStack(path: $router.hostsPath) {
                    HostsTabRoot(router: self.router, stores: stores)
                        .navigationDestination(for: Route.self) { page($0) }
                }
                .tabSurface(selected: router.tab == .hosts)

                NavigationStack(path: $router.youPath) {
                    YouTabRoot(
                        router: self.router, accounts: accounts, stores: stores,
                        deletion: deletion, removal: removal, appearance: appearance,
                        report: report, actions: actions)
                        .navigationDestination(for: Route.self) { page($0) }
                }
                .tabSurface(selected: router.tab == .you)
            }

            if router.path.isEmpty {
                ShellTabBar(selected: router.tab) { router.select($0) }
                    .safeAreaPadding(.bottom, 6)
            }
        }
        .tint(design.accentColor)
        .reported("shell", value: router.tab.rawValue)
        .simultaneousGesture(backGesture)
    }

    /// Restores the platform's edge-to-pop interaction while the custom page
    /// chrome keeps the system navigation bar out of the approved layout.
    private var backGesture: some Gesture {
        DragGesture(minimumDistance: 12, coordinateSpace: .global)
            .onEnded { value in
                guard !router.path.isEmpty,
                      value.startLocation.x <= 24,
                      value.translation.width >= 80,
                      abs(value.translation.width) > abs(value.translation.height)
                else { return }
                router.pop()
            }
    }

    /// One page per route. A route with no screen behind it yet says so rather
    /// than showing something that looks like the screen it is not.
    @ViewBuilder
    private func page(_ route: Route) -> some View {
        switch route {
        case .conversation(let agent):
            ConversationPage(
                agent: agent, router: router, stores: stores, recording: recording,
                actions: actions)
        case .changes(let agent):
            ChangesPage(agent: agent, router: router, stores: stores)
        case .newAgent:
            NewAgentPage(router: router, stores: stores)
        case .pairByCode(let host):
            PairByCodePage(host: host, router: router, stores: stores, actions: actions)
        case .pairConfirmation(let invitation):
            PairConfirmationPage(
                invitation: invitation, router: router, stores: stores, actions: actions)
        case .signIn(let from):
            SignInPage(from: from, router: router, model: signIn, actions: actions)
        case .paywall(let from):
            PaywallPage(from: from, router: router, model: paywall, actions: actions)
        default:
            UnbuiltPage(route: route)
        }
    }
}

private extension View {
    /// Retains every root screen and its navigation state while exposing only
    /// the selected one to drawing, input, and accessibility.
    func tabSurface(selected: Bool) -> some View {
        opacity(selected ? 1 : 0)
            .allowsHitTesting(selected)
            .accessibilityHidden(!selected)
    }
}

/// The app's three top-level places. It floats above root screens and leaves
/// pushed work alone so a conversation ends with its composer.
private struct ShellTabBar: View {
    let selected: Tab
    let select: (Tab) -> Void

    var body: some View {
        TabChrome {
            ForEach(Tab.allCases, id: \.self) { item in
                let isSelected = item == selected
                Button { select(item) } label: {
                    TabLabel(item.title, glyph: item.symbol, selected: isSelected)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.amuxControl)
                .frame(maxWidth: .infinity)
                .accessibilityLabel(item.title)
                .accessibilityAddTraits(isSelected ? .isSelected : [])
                .identified(
                    "tab.\(item.rawValue)", label: item.title,
                    value: isSelected ? "selected" : "not selected")
            }
        }
    }
}

/// One agent's conversation, and everything it asks of the app around it.
private struct ConversationPage: View {
    let agent: AgentId
    let router: Router
    let stores: StoreBundle
    /// Where a report of this conversation is recorded, and where a replay of
    /// one left what it is to be put back showing. Nothing in the shipping app.
    let recording: ConversationRecording?
    /// What leaves this page and the shell entirely — buying the relay tunnel
    /// to the machine this conversation runs on, which is nothing a
    /// conversation or a router can do.
    let actions: @MainActor (ShellAction) -> Void
    @Environment(\.scenePhase) private var scenePhase
    @State private var dictation = SpeechDictation()
    /// The system's own pickers, asked for from the plus. They are presented
    /// here rather than from the conversation because they are the system's
    /// screens: a conversation that could raise one could not be photographed
    /// or replayed away from a device.
    @State private var pickingPhoto = false
    @State private var pickingFile = false
    @State private var picked: PhotosPickerItem?
    init(
        agent: AgentId, router: Router, stores: StoreBundle,
        recording: ConversationRecording?,
        actions: @escaping @MainActor (ShellAction) -> Void
    ) {
        self.agent = agent
        self.router = router
        self.stores = stores
        self.recording = recording
        self.actions = actions
    }

    var body: some View {
        let model = stores.conversation(agent)
        Conversation(
            model: model,
            subject: ConversationSubject(agent: agent, in: stores.fleet, retaining: model),
            showing: recording?.showing[agent].flatMap(ConversationOverlay.init(rawValue:)),
            resting: recording?.reading[agent],
            opening: recording.map { recording in
                { @MainActor @Sendable in recording.opened?(agent, $0?.rawValue) }
            },
            reading: recording.map { recording in
                { @MainActor @Sendable in recording.read?(agent, $0) }
            },
            naming: { stores.fleet.name(of: $0) },
        ) { action in
            switch action {
            // The same place the edge swipe goes: the page this one was
            // pushed from, which is the Agents list for any conversation
            // not reached from its parent.
            case .back: router.pop()
            case .openChanges: router.open(.changes(agent))
            // The overflow opens over the conversation, which is the
            // conversation's own doing; nothing is pushed.
            case .overflow: break
            // Asking again means asking this phone's own link to the
            // relay, not the machine: nothing on the far side of a
            // connection that is down can be asked anything. It shortens
            // the wait the connection is already in and nothing more, so
            // pressing it repeatedly is one attempt.
            case .retry: stores.retryNow()
            // Not a retry. The machine is answering the relay perfectly
            // well and the relay will not carry anything to it on this
            // account, so what is offered is the subscription and the
            // page that sells it is the one every other offer opens.
            case .subscribe: actions(.subscribe)
            // Answering is the one thing on this screen that leaves the
            // phone. The panel spells the command, because only it knows
            // which ask this is and which layer raised it; the bundle
            // sends it and keeps the operation, so the host's reply
            // belongs to this conversation.
            case .answer(let panel, let decision):
                stores.answer(panel, decision, of: agent)
            // A child is pushed on top of its parent rather than replacing
            // it, so answering the child and coming back finds the parent
            // where it was left — the page underneath is never torn down.
            case .openChild(let child):
                stores.fleet.opened(child)
                router.open(.conversation(child))
            // Writing to an agent is the other thing on this screen that
            // leaves the phone. The bundle decides whether the layer will
            // take the message now or has to hold it, because the bundle
            // has the gate; the screen only says that the person pressed.
            case .send:
                dictation.stop()
                stores.send(to: agent)
            case .interrupt: stores.interrupt(agent)
            // Taking the held message back is a write too: the host is
            // holding it and only the host can stop holding it. The
            // bundle puts the text in the field before it dispatches, so
            // a refusal leaves the paragraph in front of whoever wrote it.
            case .unqueue: stores.unqueue(agent)
            // Opening the plus is the conversation's own state; the two
            // tiles inside it are the system's screens, raised from here.
            // Permissions is neither: it opens as a card in the
            // conversation, which the conversation has already done.
            case .attaching(.photo): pickingPhoto = true
            case .attaching(.file): pickingFile = true
            case .attach, .attaching(.permissions): break
            case .dictate: dictation.toggle(stores.conversation(agent))
            case .dictationSettings:
                if let url = URL(string: UIApplication.openSettingsURLString) {
                    UIApplication.shared.open(url)
                }
            // Picking a command is a change to the draft the conversation
            // already made, and the draft is what a send carries: there is
            // nothing here to do about it that sending will not do.
            case .picking: break
            // How this agent runs is the layer's to decide and the host's
            // to keep. The bundle spells each change in the provider's own
            // vocabulary — Claude has a mode, Codex has a pair of axes —
            // and refuses one the layer said it would refuse, which is the
            // same sentence the sheet is already printing.
            case .setting(let change):
                switch change {
                case .model(let model): stores.setModel(model, of: agent)
                case .effort(let effort): stores.setEffort(effort, of: agent)
                case .permission(let choice): stores.setPermission(choice, of: agent)
                }
            // Opening the sheet is the conversation's own state; there is
            // nothing outside it that has to know.
            case .openSettings: break
            // Copying is the one thing on this screen that goes to the
            // system rather than to a host. The address travels with the
            // choice, so what lands on the clipboard is the string the row
            // showed and not a second spelling made here.
            case .overflowing(let choice):
                switch choice {
                case .copyAddress(let address): copy(address)
                case .rename, .delete: break
                }
            case .renamed(let name): stores.rename(name, of: agent)
            // Asking is not the same as it having happened. The write goes
            // out and the screen stays; leaving is what the confirmation
            // below does, when the host says the agent is gone.
            case .deleteAgent: stores.delete(agent)
            }
        }
        // Replacing one conversation route with another keeps the same
        // destination type. Key its ephemeral overlay and scroll state to
        // the agent so a long transcript never inherits the position and
        // geometry callbacks of the conversation it replaced. Drafts and
        // transcript data live in their per-agent stores and survive.
        .id(agent)
        // A conversation has no bar. The feed runs to the top of the display
        // and the way out is the back chevron on its own chrome.
        .toolbar(.hidden, for: .navigationBar)
        .onDisappear { dictation.stop() }
        .onChange(of: scenePhase) { _, phase in
            if phase == .background { dictation.stop() }
        }
        .onChange(of: stores.conversation(agent).gate) { _, gate in
            if ComposerState(gate: gate, tail: nil, elapsed: nil) == nil { dictation.stop() }
        }
        // A deleted agent has no conversation to be in. Leaving is driven by
        // the host's confirmation rather than by the press, so a deletion the
        // host refused leaves the person where they were, reading why.
        .onChange(of: stores.conversation(agent).deleted) { _, gone in
            if gone {
                router.pop()
                stores.closeConversation(agent)
            }
        }
        .photosPicker(isPresented: $pickingPhoto, selection: $picked, matching: .images)
        .onChange(of: picked) { _, item in
            guard let item else { return }
            picked = nil
            Task { await store(item) }
        }
        // Everything, because what an agent is being shown is not this app's
        // business to narrow: a person attaching a font file to ask about a
        // font file is doing something ordinary.
        .fileImporter(isPresented: $pickingFile, allowedContentTypes: [.item]) { result in
            guard case .success(let url) = result else { return }
            store(url)
        }
    }

    /// Reads a picked photograph and sends its bytes.
    ///
    /// The library gives no filename — a picker that never asked for access to
    /// the whole library cannot know one — so the name is made from the type
    /// that came back, which is the honest thing to call it.
    private func store(_ item: PhotosPickerItem) async {
        guard let bytes = try? await item.loadTransferable(type: Data.self) else { return }
        let type = item.supportedContentTypes.first ?? .image
        stores.attach(
            PickedAttachment(
                agent: agent, kind: .image,
                name: "photo.\(type.preferredFilenameExtension ?? "img")",
                mime: type.preferredMIMEType ?? "application/octet-stream"),
            bytes: bytes)
    }

    /// Reads a picked file and sends its bytes.
    ///
    /// A file chosen outside this app's own container is reached only inside
    /// a security scope, and the scope is given back whether or not the read
    /// worked — an unbalanced one leaks the grant for as long as the app runs.
    private func store(_ url: URL) {
        let scoped = url.startAccessingSecurityScopedResource()
        defer { if scoped { url.stopAccessingSecurityScopedResource() } }
        guard let bytes = try? Data(contentsOf: url) else { return }
        let type = UTType(filenameExtension: url.pathExtension)
        stores.attach(
            PickedAttachment(
                agent: agent, kind: .file, name: url.lastPathComponent,
                mime: type?.preferredMIMEType ?? "application/octet-stream"),
            bytes: bytes)
    }
}

/// The changes one turn made, and the review being written about them.
///
/// The page owns no state of its own: the review store holds what is folded,
/// what a finger has hold of and everything said so far, so leaving to check
/// something in the conversation and coming back finds the review as it was.
private struct ChangesPage: View {
    let agent: AgentId
    let router: Router
    let stores: StoreBundle

    var body: some View {
        Group {
            if let review = stores.review(agent) {
                DiffPage(model: review, subject: name) { action in
                    switch action {
                    case .back: router.pop()
                    case .select(let range): review.select(range)
                    case .comment(let range, let text): review.comment(range, text)
                    case .cancelComment: review.cancel()
                    case .toggleFile(let path): review.toggle(file: path)
                    // Where the page has already gone within itself. Nothing
                    // to apply: the wheel and the file list scroll, and a
                    // scroll is not somewhere the app has been taken.
                    case .scrubTo: break
                    // Attaching hands the review to the conversation it came
                    // from and goes back there. What is said about the patch
                    // as a whole is written beside the token as ordinary
                    // prose, so the page is done once the token exists.
                    case .attachReview:
                        if let token = review.token {
                            stores.conversation(agent).draft.attach(token)
                            router.pop()
                        }
                    }
                }
            } else {
                // The changes have not arrived, or this agent offered none.
                // Said plainly rather than drawn as an empty patch.
                UnbuiltPage(route: .changes(agent))
            }
        }
        .toolbar(.hidden, for: .navigationBar)
    }

    private var name: String {
        stores.fleet.rows.first { $0.id == agent }?.name ?? agent.description
    }
}

/// The Agents tab's root, and the title menu that switches account.
///
/// The menu hangs off the title rather than a control of its own because the
/// title is what it changes: whose agents these are.
private struct AgentsTab: View {
    let router: Router
    let accounts: AccountRegistry
    let stores: StoreBundle
    let actions: @MainActor (ShellAction) -> Void

    var body: some View {
        AgentsHome(model: stores.fleet, accounts: accounts, hosts: stores.hosts) { action in
            switch action {
            case .open(let agent):
                stores.fleet.opened(agent)
                router.open(.conversation(agent))
            case .newAgent: router.open(.newAgent)
            case .openExceptions: router.select(.hosts)
            case .switchAccount(let id): actions(.selectAccount(id))
            case .addAccount: actions(.addAccount)
            case .signIn: actions(.signIn)
            case .signInAgain(let id): actions(.signInAgain(id))
            case .subscribe: actions(.subscribe)
            // Pairing lives under Hosts wherever it is started from: the page
            // it opens is that tab's, and going back from it belongs there
            // rather than on a list of agents that has none.
            case .pair(let host):
                router.select(.hosts)
                router.open(.pairByCode(host))
            // The one place the list is allowed to regroup. Data arriving
            // never reorders what a thumb is already travelling towards.
            case .refresh: stores.fleet.refreshOrder(now: stores.now())
            }
        }
        // The screen draws its own header, so the bar would be a second one.
        .toolbar(.hidden, for: .navigationBar)
    }
}

/// Signing in.
///
/// The page navigates and the composition reaches the cloud: leaving is a pop
/// either way, and the hand-off is handed out of the shell because a browser,
/// a token and an account are none of the shell's business.
private struct SignInPage: View {
    let from: Tab
    let router: Router
    let model: SignInStore
    let actions: @MainActor (ShellAction) -> Void

    var body: some View {
        SignIn(model: model, back: from.title) { action in
            switch action {
            case .cancel, .done: router.pop()
            case .start: actions(.handOffSignIn)
            case .keep: actions(.keepSignIn)
            case .discard: actions(.discardSignIn)
            }
        }
        // The screen draws its own header, so the bar would be a second one.
        .toolbar(.hidden, for: .navigationBar)
    }
}

/// Subscribing.
///
/// Choosing a plan is the screen's own state and is settled here; buying and
/// restoring leave the shell, because both are the App Store's and the shell
/// reaches nothing.
private struct PaywallPage: View {
    let from: Tab
    let router: Router
    let model: PaywallStore
    let actions: @MainActor (ShellAction) -> Void

    var body: some View {
        Paywall(model: model, back: from.title) { action in
            switch action {
            case .cancel, .done: router.pop()
            case .choose(let period): model.choose(period)
            case .buy: actions(.buySubscription)
            case .restore: actions(.restorePurchases)
            case .retry: actions(.retryPurchase)
            }
        }
        // The screen draws its own header, so the bar would be a second one.
        .toolbar(.hidden, for: .navigationBar)
    }
}

private struct HostsTabRoot: View {
    let router: Router
    let stores: StoreBundle

    var body: some View {
        HostsTab(model: stores.hosts) { action in
            switch action {
            case .open(let host): router.open(.host(host))
            case .pair(let host): router.open(.pairByCode(host))
            case .newAgent: router.open(.newAgent)
            // Revoking goes nowhere: the list it happens on is the list it
            // changes, and the machine leaves it when the runtime says so.
            case .revoke(let host): stores.revoke(host)
            // A local network this phone refused is granted back in the
            // system's settings and nowhere else: iOS asks once, and this app
            // has no second ask to offer.
            case .openSystemSettings:
                if let url = URL(string: UIApplication.openSettingsURLString) {
                    UIApplication.shared.open(url)
                }
            }
        }
        // The screen draws its own header, so the bar would be a second one.
        .toolbar(.hidden, for: .navigationBar)
    }
}

/// Starting an agent on one of this phone's machines.
///
/// The page opens the attempt rather than the screen doing it, and asks the
/// machine it opened on what it has to offer straight away: the answer takes a
/// round trip and the screen is useful before it arrives, so nothing waits on
/// it.
private struct NewAgentPage: View {
    let router: Router
    let stores: StoreBundle

    var body: some View {
        NewAgent(model: stores.newAgent, hosts: stores.hosts) { action in
            switch action {
            // A different machine means a different set of directories, so
            // pointing at one asks it what it has.
            case .point(let host): stores.point(at: host)
            case .search: stores.searchDirectories()
            // Starting is the one thing on this screen that leaves the phone.
            // Leaving is what the confirmation below does, when the machine
            // says the agent exists — a screen that left on the press would be
            // claiming something it has not been told.
            case .start: stores.startAgent()
            case .cancel: router.pop()
            }
        }
        .toolbar(.hidden, for: .navigationBar)
        .onAppear { stores.startNewAgent(on: stores.hosts.online.first?.id) }
        // The conversation replaces this page rather than sitting on top of it:
        // going back from an agent that was just started belongs at the list it
        // joined, not at the form that made it.
        .onChange(of: stores.newAgent.created) { _, started in
            guard let started else { return }
            stores.fleet.opened(started.id)
            router.show(.conversation(started.id))
        }
    }
}

/// Typing a machine's six-digit code, and then deciding about the machine that
/// answered it.
///
/// The page opens the attempt rather than the screen doing it, because opening
/// one is what clears the last one: a refusal left on screen from the code
/// somebody typed a minute ago would be read as this code failing.
///
/// A code that authenticates leads to the same confirmation a link leads to,
/// on this page rather than on another one. Pairing is two acts on purpose —
/// knowing the code proves possession of the offer and nothing about which
/// machine made it — so the digits are never the end of it: what the machine
/// said its name and its key are has to be read and agreed to before any trust
/// is written. Keeping both halves here means the person who typed the code
/// stays where they typed it, and going back from either is going back to
/// Hosts.
private struct PairByCodePage: View {
    let host: HostId
    let router: Router
    let stores: StoreBundle
    /// Buying the relay tunnel, for the one refusal a code cannot be typed out
    /// of: the machine answered and this account may not open one to it.
    let actions: @MainActor (ShellAction) -> Void

    var body: some View {
        page
            .toolbar(.hidden, for: .navigationBar)
            .onAppear { stores.pairing.open(machine: stores.hosts.known(host)) }
    }

    @ViewBuilder
    private var page: some View {
        switch stores.pairing.phase {
        // A refusal belongs to the digits: it is the code that did not work,
        // and the next thing to do is type another one.
        case .entering, .checking, .refused, .needsSubscription: digits
        case .confirming, .trusted: decision
        }
    }

    private var digits: some View {
        PairByCode(model: stores.pairing) { action in
            switch action {
            case .digits(let typed): stores.pair(digits: typed)
            // A code cannot reach any of these — nothing on the keypad
            // authenticates, and an account is not what a typed code needs.
            case .confirm, .abandon, .signIn: break
            case .subscribe: actions(.subscribe)
            case .cancel: router.pop()
            }
        }
    }

    private var decision: some View {
        PairConfirmation(model: stores.pairing) { action in
            switch action {
            case .confirm(let peer): stores.confirmPairing(peer)
            // Turning the machine away is a message to it, not just a way off
            // the screen: told, it can release the attempt now rather than
            // holding it open until it expires. What is left behind is an
            // empty keypad, because the next thing somebody does after
            // refusing a machine is try the code for the right one.
            case .abandon(let peer): stores.abandonPairing(peer)
            case .cancel: router.pop()
            case .digits, .signIn, .subscribe: break
            }
        }
    }
}

/// Where a pairing link lands.
///
/// Arriving here authenticates the invitation and nothing else. The machine is
/// asked to prove it issued the link and to say who it is; the trust is written
/// only when the person presses, and leaving instead tells the machine so.
///
/// The authentication is asked for whenever this page has something to ask
/// with, which is what carries a link across a cold start and a sign-in: a
/// launch that opened on this page with no account yet asks again the moment
/// an account arrives, rather than stranding the person on a screen that can
/// never say who it is confirming.
private struct PairConfirmationPage: View {
    let invitation: PairingInvitation
    let router: Router
    let stores: StoreBundle
    let actions: @MainActor (ShellAction) -> Void
    @State private var asked = LinkAsked()

    var body: some View {
        page
            .toolbar(.hidden, for: .navigationBar)
            .onAppear { ask() }
            .onChange(of: stores.account) { _, _ in ask() }
            .onChange(of: stores.fleet.connection.state) { _, _ in ask() }
    }

    /// A machine only the relay has seen, reached by a phone with no relay, is
    /// one missing piece rather than a failed invitation. The invitation is
    /// kept — this page is still the route, and signing in makes it ask — so
    /// nothing is spent by saying so.
    @ViewBuilder
    private var page: some View {
        if unreachable {
            PairNeedsAnAccount { action in
                switch action {
                case .signIn: actions(.signIn)
                case .cancel: router.pop()
                case .confirm, .abandon, .digits, .subscribe: break
                }
            }
        } else {
            confirmation
        }
    }

    /// Whether this invitation has nothing this phone can act on: no address
    /// to dial and no account to reach a relay with.
    private var unreachable: Bool {
        invitation.needsAnAccount && stores.hosts.cloud == .signedOut
    }

    private var confirmation: some View {
        PairConfirmation(model: stores.pairing) { action in
            switch action {
            case .confirm(let peer): stores.confirmPairing(peer)
            // Turning the machine away is a message to it, not just a way off
            // the screen: told, it can release the attempt now instead of
            // holding it open until it expires.
            case .abandon(let peer):
                stores.abandonPairing(peer)
                router.pop()
            case .cancel: router.pop()
            case .digits, .signIn: break
            case .subscribe: actions(.subscribe)
            }
        }
    }

    /// Puts the invitation to the machine, once there is anything to put it
    /// with.
    ///
    /// A link is authenticated over the relay it names, so a phone with no
    /// account and a phone whose connection has not finished opening are the
    /// same thing here: there is nobody to ask. Asking anyway comes back as a
    /// refusal within the second, and "that invitation did not work" is a
    /// verdict about the machine — saying it about a connection that was still
    /// being made would send somebody back to a machine that is fine to ask it
    /// for another code.
    private func ask() {
        // An invitation carrying addresses is dialled on the network this
        // phone is already on: there is no relay in the way of it, so waiting
        // for one would strand a machine standing on the same desk behind an
        // account nobody needs.
        guard !invitation.needsAnAccount || stores.fleet.connection.state == .connected,
              !unreachable,
              asked.shouldAsk(stores.account)
        else { return }
        stores.pairing.open()
        if stores.pair(link: invitation.payload) { asked.asked(stores.account) }
    }
}

/// Which account a pairing invitation has been put to.
///
/// A link can arrive before this phone has an account at all: a cold start
/// hands the URL over before the first frame, and the person may sign in
/// afterwards. That does not spend the invitation — it was never asked about —
/// so it is put to whichever account is on show, once each. Signing in asks;
/// an ordinary redraw does not; and switching to a second account asks again,
/// because trust is per account and the first account's answer is not the
/// second's.
struct LinkAsked {
    private var account: AccountId?

    func shouldAsk(_ current: AccountId) -> Bool { account != current }

    mutating func asked(_ current: AccountId) { account = current }
}

/// You.
///
/// The screen decides nothing: switching account, signing in and out, buying
/// and deleting all leave here, because each of them either changes what the
/// whole app is pointed at or reaches something outside it.
private struct YouTabRoot: View {
    let router: Router
    let accounts: AccountRegistry
    let stores: StoreBundle
    let deletion: DeletionStore
    let removal: RemovalStore
    let appearance: Appearance?
    /// Freezes the screen behind this page and opens the report on it.
    let report: @MainActor () -> Void
    let actions: @MainActor (ShellAction) -> Void

    var body: some View {
        DeleteAccountOverlay(
            entry: accounts.selectedAccount, model: deletion,
            actions: { asked in
                switch asked {
                case .cancel: actions(.cancelDeletion)
                case .confirm: actions(.confirmDeletion)
                // The only place a renewal can be stopped is where it was
                // bought, which for an App Store subscription is a page of the
                // system's rather than one this app or amux.sh owns.
                case .manageBilling(let url): leave(for: url)
                }
            }
        ) {
            RemoveAccountOverlay(
                accounts: accounts, model: removal,
                actions: { asked in
                    switch asked {
                    case .cancel: actions(.cancelRemoval)
                    case .confirm: actions(.confirmRemoval)
                    }
                }
            ) {
                you
            }
        }
        // The screen draws its own header, so the bar would be a second one.
        .toolbar(.hidden, for: .navigationBar)
    }

    private var you: some View {
        YouScreen(
            accounts: accounts, appearance: appearance,
            // This phone's own key, read off the machine store the way the
            // devices page reads it: absent until a connection has said what
            // this device's identity is, rather than guessed at.
            identity: stores.hosts.roster.map { Fingerprint.short($0.identity.fingerprint) }
        ) { action in
            switch action {
            case .select(let id): actions(.selectAccount(id))
            case .add: actions(.addAccount)
            case .signIn(let id): actions(.signInAgain(id))
            case .signOut(let id): actions(.signOutAccount(id))
            case .remove(let id): actions(.removeAccount(id))
            case .subscription: actions(.subscribe)
            case .appearance(let wanted): actions(.wear(wanted))
            case .delete(let id): actions(.deleteAccount(id))
            // This phone's key and the machines that trust it are one page,
            // and it is the machines tab: an identity is only interesting
            // beside what it is trusted by.
            case .identity: router.select(.hosts)
            // Reaching a person happens on the web, where the people are.
            // There is no form in here to fill in: a message written into this
            // app would have to be carried by the same account service the
            // person may be writing about because they cannot reach it.
            case .support: leave(for: CloudEndpoint.production.support)
            // Writing a report freezes the frame that was on show before any
            // of the report's own UI appears — otherwise the picture would be
            // of the report rather than of what was wrong.
            case .report: report()
            case .dismiss: break
            }
        }
    }
}
