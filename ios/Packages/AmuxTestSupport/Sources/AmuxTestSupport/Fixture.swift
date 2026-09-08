import AmuxCore
import AmuxFeatures
import Foundation

/// A named state the app can be put into without a network or a host.
///
/// A fixture is the one place a state is written down: goldens capture it, the
/// driving door opens it, and unit tests load it into fresh stores. Three
/// things naming the same state cannot disagree about what it is.
public struct Fixture: Identifiable, Sendable {
    public let id: String
    public let screen: Screen
    /// What the cloud answers while this state is on screen.
    public let cloud: ScriptedCloudState
    /// The accounts this phone knows in this state. Most states are one
    /// signed-in, subscribed account; the two gated states are the ones that
    /// are not, and they are the reason this is stated rather than assumed.
    public let accounts: [AccountEntry]
    /// What the App Store answers while this state is on screen.
    public let store: ScriptedStoreState
    /// Where a purchase stands in this state. Like the sign-in phase, it is
    /// declared rather than reached, because a store sheet belongs to another
    /// process and cannot be pressed from here.
    public let paywall: PaywallStore.Phase
    /// The account this state is in the middle of giving up, if any. Nothing
    /// means nobody has been asked about, which is every state but the two the
    /// question itself is the subject of.
    public let deletion: Deleting?
    /// Where a sign-in stands in this state. It is declared here rather than
    /// applied to a bundle because it is not an account's fact: signing in is
    /// what makes an account, so there is none to hang it on yet.
    public let signIn: SignInStore.Phase
    /// The type size to render at, in the door's own words. Absent means the
    /// device's own setting.
    public let typeSize: String?
    /// What the conversation has opened over itself in this state, where the
    /// screen name does not say. Two states are the settings screen — the
    /// model sheet and the permissions sheet — so which one is a fact about
    /// the state rather than about the screen.
    public let overlay: ConversationOverlay?
    /// Fills stores directly. A fixture never speaks a protocol: a journey
    /// that claims protocol coverage drives the real relay instead.
    public let apply: @Sendable @MainActor (StoreBundle) -> Void

    public init(
        id: String,
        screen: Screen,
        cloud: ScriptedCloudState = ScriptedCloudState(),
        accounts: [AccountEntry] = [Fixture.subscribed],
        signIn: SignInStore.Phase = .ready,
        store: ScriptedStoreState = ScriptedStoreState(),
        paywall: PaywallStore.Phase = .ready,
        deletion: Deleting? = nil,
        typeSize: String? = nil,
        overlay: ConversationOverlay? = nil,
        apply: @escaping @Sendable @MainActor (StoreBundle) -> Void = { _ in }
    ) {
        self.id = id
        self.screen = screen
        self.cloud = cloud
        self.accounts = accounts
        self.signIn = signIn
        self.store = store
        self.paywall = paywall
        self.deletion = deletion
        self.typeSize = typeSize
        self.overlay = overlay
        self.apply = apply
    }

    /// A deletion in progress, as a state declares it: whose account, what has
    /// been typed to confirm it, and what the account service has answered.
    /// Declared rather than performed, because the card is only on screen once
    /// somebody has pressed the row that opens it.
    public struct Deleting: Sendable, Equatable {
        public var account: AccountId
        public var typed: String
        public var phase: DeletionStore.Phase

        public init(
            account: AccountId, typed: String = "",
            phase: DeletionStore.Phase = .asking
        ) {
            self.account = account
            self.typed = typed
            self.phase = phase
        }
    }

    /// The account every state assumes unless it is about not having one.
    public static let subscribed = AccountEntry(
        account: ScriptedCloudState.ada,
        entitlement: .active(source: .web, renews: nil))

    /// Signed in, nothing bought.
    public static let unsubscribed = AccountEntry(account: ScriptedCloudState.ada)

    /// A phone with more than one account on it: the person's own, the work
    /// one they are also in, and one they signed out of and kept.
    ///
    /// Three because that is what the switcher has to cope with — a selected
    /// account, another signed-in one, and one offering to sign back in. The
    /// host counts are the ones a connection reported; the account nobody is
    /// connected to has none, and says its address instead.
    public static let several = [
        AccountEntry(
            account: ScriptedCloudState.ada,
            entitlement: .active(source: .web, renews: nil), hosts: 3),
        AccountEntry(
            account: SignedInAccount(
                id: AccountId("acme"), email: "ada@acme.example", displayName: "Acme"),
            entitlement: .active(source: .appStore, renews: nil)),
        AccountEntry(
            account: SignedInAccount(
                id: AccountId("side"), email: "side@example.com", displayName: "Side project"),
            signedIn: false),
    ]
}
