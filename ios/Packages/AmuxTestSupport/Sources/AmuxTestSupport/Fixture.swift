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
        self.typeSize = typeSize
        self.overlay = overlay
        self.apply = apply
    }

    /// The account every state assumes unless it is about not having one.
    public static let subscribed = AccountEntry(
        account: ScriptedCloudState.ada,
        entitlement: .active(source: .web, renews: nil))

    /// Signed in, nothing bought.
    public static let unsubscribed = AccountEntry(account: ScriptedCloudState.ada)
}
