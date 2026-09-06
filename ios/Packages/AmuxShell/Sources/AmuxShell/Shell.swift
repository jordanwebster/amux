import AmuxCore
import AmuxDesign
import AmuxFeatures
import SwiftUI

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

    var body: some View {
        DrawerOverlay(open: $open, drawer: drawer) {
            Conversation(
                model: stores.conversation(agent),
                subject: ConversationSubject(agent: agent, in: stores.fleet),
                naming: { child in
                    stores.fleet.rows.first { $0.id == child }?.name ?? child.description
                }
            ) { action in
                switch action {
                case .openDrawer: open = true
                case .openChanges: router.open(.changes(agent))
                // The overflow's own panel is not built yet, so nothing is
                // presented and nothing pretends to have been.
                case .overflow: break
                // Asking the machine again means asking the runtime, and the
                // shell has no runtime to ask yet. Reconnecting on its own
                // schedule is what is already happening, which is what the
                // panel says; the button is here so the offer is on the screen
                // it belongs to rather than arriving with the wiring.
                case .retry: break
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
                }
            }
        }
        // A conversation has no bar. The feed runs to the top of the display
        // and the way out is the drawer control on its own chrome.
        .toolbar(.hidden, for: .navigationBar)
    }

    private var drawer: AgentsDrawer {
        AgentsDrawer(model: stores.fleet, hosts: stores.hosts, current: agent) { action in
            open = false
            switch action {
            case .open(let other):
                stores.fleet.opened(other)
                router.open(.conversation(other))
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
        HostsPlaceholder(router: router, stores: stores)
            .navigationTitle(Tab.hosts.title)
    }
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
