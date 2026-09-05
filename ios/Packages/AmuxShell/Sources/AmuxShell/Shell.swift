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
            ConversationPlaceholder(agent: agent, router: router, stores: stores)
        default:
            UnbuiltPage(route: route)
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
        FleetPlaceholder(router: router, stores: stores)
            .navigationTitle(title)
            .toolbarTitleMenu {
                ForEach(accounts.accounts) { entry in
                    Button {
                        actions(.selectAccount(entry.id))
                    } label: {
                        Label(entry.account.email, systemImage: entry.id == accounts.selected
                            ? "checkmark" : "person")
                    }
                    .accessibilityIdentifier("titleMenu.account.\(entry.account.email)")
                }
                Button("Add Account") { actions(.addAccount) }
                    .accessibilityIdentifier("titleMenu.addAccount")
            }
    }

    /// The account whose fleet this is, when there is more than one to be
    /// confused about; the tab's own name when there is not.
    private var title: String {
        guard accounts.accounts.count > 1, let selected = accounts.selectedAccount else {
            return Tab.agents.title
        }
        return selected.account.displayName ?? selected.account.email
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
