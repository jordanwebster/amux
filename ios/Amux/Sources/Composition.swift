import AmuxCore
import AmuxShell
import Foundation
import Observation

/// Everything the app is made of, assembled in one place.
///
/// The screens know their stores, the shell knows where things lead, and this
/// knows which stores and which shell — so switching account swaps a set of
/// stores here and nothing above has to be told about accounts at all.
@MainActor
@Observable
final class Composition {
    let accounts = AccountRegistry()
    let router = Router()

    /// Where a fleet goes before anybody has signed in. The app runs signed
    /// out — it shows an empty home rather than a login wall — so there has to
    /// be somewhere for a cache to land that is not an account's.
    private let signedOut = StoreBundle(account: AccountId("signed-out"))

    var stores: StoreBundle { accounts.stores ?? signedOut }

    init() {
        router.loads(with: self)
        rememberedFleet()
    }

    /// Puts the fleet this phone saw last time on screen before anything has
    /// been reached.
    ///
    /// Read straight off disk by the shared library rather than by starting the
    /// runtime first: a launch has rows to draw long before it has a network,
    /// and a person opening the app to check on an agent should not watch an
    /// empty screen while a connection is negotiated. Every row arrives marked
    /// as remembered, and each one goes solid when the machine that owns it
    /// answers.
    private func rememberedFleet() {
        stores.apply(Bridge.cachedFleet(in: AppFiles.cache))
    }

    /// What the shell asks for that it cannot do itself.
    func handle(_ action: ShellAction) {
        switch action {
        case .selectAccount(let id):
            accounts.select(id)
        // Signing in, adding an account and subscribing all leave the app for
        // the web or the store. Until those journeys are built there is
        // nowhere to send somebody, and inventing a local sign-in that the
        // real one would have to undo would be worse than the button doing
        // nothing.
        case .addAccount, .signIn, .subscribe:
            break
        }
    }
}

extension Composition: RouteLoader {
    /// Fills a page that is already on screen.
    ///
    /// Opening a conversation opens its store, which is what tells the runtime
    /// this client is watching that agent. The transcript arrives afterwards
    /// and lands in the page the tap already pushed.
    func load(_ route: Route) {
        switch route {
        case .conversation(let agent), .changes(let agent):
            stores.conversation(agent)
        case .newAgent, .pairByCode, .pairConfirmation, .host, .accounts, .appearance, .help:
            break
        }
    }
}

/// Where this app keeps things between launches.
///
/// The fleet is a cache: losing it costs one launch its remembered rows and
/// nothing else, so it lives where the system is allowed to reclaim it. The
/// shared runtime is handed the same two directories, so what a launch reads
/// and what a connection writes are one file.
enum AppFiles {
    static let support = directory(.applicationSupportDirectory)
    static let cache = directory(.cachesDirectory)

    private static func directory(_ search: FileManager.SearchPathDirectory) -> URL {
        let manager = FileManager.default
        let root = manager.urls(for: search, in: .userDomainMask)[0]
            .appendingPathComponent("amux", isDirectory: true)
        try? manager.createDirectory(at: root, withIntermediateDirectories: true)
        return root
    }
}
