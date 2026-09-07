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
    case signIn
    case subscribe
}

/// The app: three tabs, a stack under each, and a title menu on the Agents
/// tab that names the account whose fleet is on show.
///
/// Everything here is navigation and composition. The screens themselves are
/// functions of their stores and never reach for a route, which is why one can
/// be captured or replayed on its own.
public struct Shell: View {
    private let router: Router
    private let accounts: AccountRegistry
    private let stores: StoreBundle
    private let actions: @MainActor (ShellAction) -> Void

    public init(
        router: Router,
        accounts: AccountRegistry,
        stores: StoreBundle,
        actions: @escaping @MainActor (ShellAction) -> Void
    ) {
        self.router = router
        self.accounts = accounts
        self.stores = stores
        self.actions = actions
    }

    public var body: some View {
        @Bindable var router = router
        TabView(selection: tab) {
            SwiftUI.Tab(Tab.agents.title, systemImage: Tab.agents.symbol, value: Tab.agents) {
                NavigationStack(path: $router.agentsPath) {
                    AgentsTab(router: self.router, accounts: accounts, stores: stores, actions: actions)
                        .navigationDestination(for: Route.self) { page($0) }
                }
            }
            SwiftUI.Tab(Tab.hosts.title, systemImage: Tab.hosts.symbol, value: Tab.hosts) {
                NavigationStack(path: $router.hostsPath) {
                    HostsTabRoot(router: self.router, stores: stores)
                        .navigationDestination(for: Route.self) { page($0) }
                }
            }
            SwiftUI.Tab(Tab.you.title, systemImage: Tab.you.symbol, value: Tab.you) {
                NavigationStack(path: $router.youPath) {
                    YouTabRoot(router: self.router, accounts: accounts, actions: actions)
                        .navigationDestination(for: Route.self) { page($0) }
                }
            }
        }
        // The tab bar carries no name of this app's. An identifier put on a
        // `Tab` lands on the page behind it rather than on the button in the
        // bar, so naming them here would read as a contract that nothing can
        // keep; the bar is the system's control and is reached by its title,
        // the way a person reads it. What the shell does state is which tab is
        // showing.
        .identified("shell", value: router.tab.rawValue)
    }

    /// The tab bar, written through the router rather than straight into it.
    ///
    /// Reaching for the tab you are already on is the platform's way of saying
    /// "take me back to the top of this", and it is the only way out of a
    /// conversation now that a conversation has no bar to go back from. A
    /// plain binding to the stored property would never see that tap, because
    /// the value it sets is the value already there.
    private var tab: Binding<Tab> {
        Binding(get: { router.tab }, set: { router.select($0) })
    }

    /// One page per route. A route with no screen behind it yet says so rather
    /// than showing something that looks like the screen it is not.
    @ViewBuilder
    private func page(_ route: Route) -> some View {
        switch route {
        case .conversation(let agent):
            ConversationPage(agent: agent, router: router, stores: stores)
        case .changes(let agent):
            ChangesPage(agent: agent, router: router, stores: stores)
        case .pairByCode(let host):
            PairByCodePage(host: host, router: router, stores: stores)
        case .pairConfirmation(let invitation):
            PairConfirmationPage(invitation: invitation, router: router, stores: stores)
        default:
            UnbuiltPage(route: route)
        }
    }
}

/// One agent's conversation with the drawer over it.
///
/// The drawer is drawn here rather than inside the conversation because it is
/// not part of the conversation: it is the fleet, borrowing the screen. Wrapped
/// this way the page underneath is never torn down, so closing the drawer
/// returns to the same conversation at the position it was left at.
private struct ConversationPage: View {
    let agent: AgentId
    let router: Router
    let stores: StoreBundle
    /// Whose screen this is while it is out: view state, because a drawer is
    /// something this page is doing and not somewhere the app has gone.
    @State private var open = false
    /// The system's own pickers, asked for from the plus. They are presented
    /// here rather than from the conversation because they are the system's
    /// screens: a conversation that could raise one could not be photographed
    /// or replayed away from a device.
    @State private var pickingPhoto = false
    @State private var pickingFile = false
    @State private var picked: PhotosPickerItem?

    var body: some View {
        DrawerOverlay(open: $open, drawer: drawer) {
            Conversation(
                model: stores.conversation(agent),
                subject: ConversationSubject(agent: agent, in: stores.fleet),
                naming: { stores.fleet.name(of: $0) }
            ) { action in
                switch action {
                case .openDrawer: open = true
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
                case .send: stores.send(to: agent)
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
                // The system's own dictation is not wired yet. The control is
                // on the screen it belongs to rather than arriving with the
                // wiring, and it does not pretend to have run.
                case .dictate: break
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
                    if case .copyAddress(let address) = choice { copy(address) }
                case .renamed(let name): stores.rename(name, of: agent)
                // Asking is not the same as it having happened. The write goes
                // out and the screen stays; leaving is what the confirmation
                // below does, when the host says the agent is gone.
                case .deleteAgent: stores.delete(agent)
                }
            }
        }
        // A conversation has no bar. The feed runs to the top of the display
        // and the way out is the drawer control on its own chrome.
        .toolbar(.hidden, for: .navigationBar)
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

    private var drawer: AgentsDrawer {
        AgentsDrawer(model: stores.fleet, hosts: stores.hosts, current: agent) { action in
            open = false
            switch action {
            case .open(let other):
                stores.fleet.opened(other)
                router.show(.conversation(other))
            case .newAgent: router.open(.newAgent)
            case .hosts: router.select(.hosts)
            case .you: router.select(.you)
            case .dismiss: break
            }
        }
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
        AgentsHome(model: stores.fleet, accounts: accounts) { action in
            switch action {
            case .open(let agent):
                stores.fleet.opened(agent)
                router.open(.conversation(agent))
            case .newAgent: router.open(.newAgent)
            case .openExceptions: router.select(.hosts)
            case .switchAccount(let id): actions(.selectAccount(id))
            case .addAccount: actions(.addAccount)
            case .signIn: actions(.signIn)
            case .subscribe: actions(.subscribe)
            // The one place the list is allowed to regroup. Data arriving
            // never reorders what a thumb is already travelling towards.
            case .refresh: stores.fleet.refreshOrder(now: Date())
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
            }
        }
        // The screen draws its own header, so the bar would be a second one.
        .toolbar(.hidden, for: .navigationBar)
    }
}

/// Typing a machine's six-digit code.
///
/// The page opens the attempt rather than the screen doing it, because opening
/// one is what clears the last one: a refusal left on screen from the code
/// somebody typed a minute ago would be read as this code failing.
private struct PairByCodePage: View {
    let host: HostId?
    let router: Router
    let stores: StoreBundle

    var body: some View {
        PairByCode(model: stores.pairing) { action in
            switch action {
            case .digits(let typed): stores.pair(digits: typed)
            // A code cannot reach either of these — nothing on this screen
            // authenticates, so there is never an attempt here to answer.
            case .confirm, .abandon: break
            case .cancel: router.pop()
            }
        }
        .toolbar(.hidden, for: .navigationBar)
        .onAppear { stores.pairing.open(machine: host.flatMap { stores.hosts.known($0) }) }
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
    @State private var asked = LinkAsked()

    var body: some View {
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
            case .digits: break
            }
        }
        .toolbar(.hidden, for: .navigationBar)
        .onAppear { ask() }
        .onChange(of: stores.account) { _, _ in ask() }
    }

    private func ask() {
        guard asked.shouldAsk(stores.account) else { return }
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

private struct YouTabRoot: View {
    let router: Router
    let accounts: AccountRegistry
    let actions: @MainActor (ShellAction) -> Void

    var body: some View {
        YouPlaceholder(router: router, accounts: accounts, actions: actions)
            .navigationTitle(Tab.you.title)
    }
}
