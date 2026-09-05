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
        TabView(selection: $router.tab) {
            SwiftUI.Tab(Tab.agents.title, systemImage: Tab.agents.symbol, value: Tab.agents) {
                NavigationStack(path: $router.agentsPath) {
                    AgentsTab(router: self.router, accounts: accounts, stores: stores, actions: actions)
                        .navigationDestination(for: Route.self) { page($0) }
                }
            }
            .accessibilityIdentifier("tab.agents")
            SwiftUI.Tab(Tab.hosts.title, systemImage: Tab.hosts.symbol, value: Tab.hosts) {
                NavigationStack(path: $router.hostsPath) {
                    HostsTabRoot(router: self.router, stores: stores)
                        .navigationDestination(for: Route.self) { page($0) }
                }
            }
            .accessibilityIdentifier("tab.hosts")
            SwiftUI.Tab(Tab.you.title, systemImage: Tab.you.symbol, value: Tab.you) {
                NavigationStack(path: $router.youPath) {
                    YouTabRoot(router: self.router, accounts: accounts, actions: actions)
                        .navigationDestination(for: Route.self) { page($0) }
                }
            }
            .accessibilityIdentifier("tab.you")
        }
        .identified("shell", value: router.tab.rawValue)
    }

    /// One page per route. A route with no screen behind it yet says so rather
    /// than showing something that looks like the screen it is not.
    @ViewBuilder
    private func page(_ route: Route) -> some View {
        switch route {
        case .conversation(let agent):
            ConversationPage(agent: agent, router: router, stores: stores)
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
/// returns to the same conversation at the position it was left at, and the
/// screen that replaces the placeholder inherits all of that unchanged.
private struct ConversationPage: View {
    let agent: AgentId
    let router: Router
    let stores: StoreBundle
    /// Whose screen this is while it is out: view state, because a drawer is
    /// something this page is doing and not somewhere the app has gone.
    @State private var open = false

    var body: some View {
        DrawerOverlay(open: $open, drawer: drawer) {
            ConversationPlaceholder(agent: agent, router: router, stores: stores)
        }
        .toolbar {
            // Where the drawer is opened from until the conversation's own
            // floating pill carries the control.
            ToolbarItem(placement: .topBarLeading) {
                Button { open = true } label: {
                    Image(systemName: "sidebar.left")
                }
                .accessibilityLabel("Agents")
                .identified("conversation.drawer", label: "Agents")
            }
        }
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
