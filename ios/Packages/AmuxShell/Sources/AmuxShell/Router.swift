import AmuxCore
import Foundation
import Observation

/// What the shell asks for once a page has been pushed.
///
/// It is called after the push and its work is never awaited before one. A
/// navigation that waited for a fetch would leave the tap looking dropped for
/// as long as the host took to answer, and would make the app's
/// responsiveness a property of the network. The page goes up first; what
/// belongs on it loads into place.
@MainActor
public protocol RouteLoader: AnyObject {
    func load(_ route: Route)
}

/// Where the app is, and the only thing that changes it.
///
/// Screens do not navigate. They say what happened and this decides where that
/// leads, so the same screen can be pushed, presented or captured without
/// carrying a route with it.
@MainActor
@Observable
public final class Router {
    /// The tab on show. Each tab keeps its own stack, so coming back to one
    /// finds it where it was left.
    public var tab: Tab = .agents

    /// One stack per tab, written to by the shell's navigation as well as
    /// read: the back gesture and the tab bar are the system's to drive, and
    /// what they did has to land somewhere the app can see.
    public var agentsPath: [Route] = []
    public var hostsPath: [Route] = []
    public var youPath: [Route] = []

    /// A link that is not navigation, kept until whatever it answers comes
    /// for it. A sign-in callback can arrive before the sign-in that started
    /// it is listening — during a cold start the system hands the link over
    /// before the first frame — and dropping it would strand the sign-in.
    public private(set) var held: DeepLink?

    private weak var loader: (any RouteLoader)?

    public init(loader: (any RouteLoader)? = nil) {
        self.loader = loader
    }

    /// Points the router at what loads a page. Set here rather than at
    /// construction when the thing that loads pages needs the router itself.
    public func loads(with loader: any RouteLoader) {
        self.loader = loader
    }

    public func path(_ tab: Tab) -> [Route] { self[keyPath: Self.stack(of: tab)] }

    /// The stack on show.
    public var path: [Route] { path(tab) }

    /// The page on show, or nothing when a tab is showing its own root.
    public var top: Route? { path.last }

    public func select(_ tab: Tab) {
        self.tab = tab
    }

    /// Pushes a page. Synchronous, and deliberately not `async`: the frame the
    /// tap happened on is the frame the page appears on.
    public func open(_ route: Route) {
        if route.tab != tab { tab = route.tab }
        self[keyPath: Self.stack(of: route.tab)].append(route)
        loader?.load(route)
    }

    /// Replaces a tab's stack. The back gesture and the tab bar write through
    /// here, which is why the stack is settable at all.
    public func setPath(_ routes: [Route], for tab: Tab) {
        self[keyPath: Self.stack(of: tab)] = routes
    }

    public func pop() {
        guard !path.isEmpty else { return }
        self[keyPath: Self.stack(of: tab)].removeLast()
    }

    public func popToRoot() {
        setPath([], for: tab)
    }

    private static func stack(of tab: Tab) -> ReferenceWritableKeyPath<Router, [Route]> {
        switch tab {
        case .agents: \.agentsPath
        case .hosts: \.hostsPath
        case .you: \.youPath
        }
    }

    /// Takes a link the app was opened with.
    ///
    /// A pairing link becomes a confirmation page and pairs with nobody: the
    /// machine on the other end is named and its fingerprint shown, and trust
    /// is committed only when the person says so. A sign-in callback is not
    /// navigation; it is held for the sign-in that started it to collect.
    @discardableResult
    public func open(_ url: URL) -> DeepLink? {
        guard let link = DeepLink(url) else { return nil }
        switch link {
        case .pair(let invitation):
            open(.pairConfirmation(invitation))
        case .signInCallback:
            held = link
        }
        return link
    }

    /// Takes the held link, if it is the kind asked for, and forgets it. A link
    /// is acted on once.
    public func takeHeld(_ isWanted: (DeepLink) -> Bool = { _ in true }) -> DeepLink? {
        guard let held, isWanted(held) else { return nil }
        self.held = nil
        return held
    }
}
