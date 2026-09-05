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
    /// The type size to render at, in the door's own words. Absent means the
    /// device's own setting.
    public let typeSize: String?
    /// Fills stores directly. A fixture never speaks a protocol: a journey
    /// that claims protocol coverage drives the real relay instead.
    public let apply: @Sendable @MainActor (StoreBundle) -> Void

    public init(
        id: String,
        screen: Screen,
        cloud: ScriptedCloudState = ScriptedCloudState(),
        accounts: [AccountEntry] = [Fixture.subscribed],
        typeSize: String? = nil,
        apply: @escaping @Sendable @MainActor (StoreBundle) -> Void = { _ in }
    ) {
        self.id = id
        self.screen = screen
        self.cloud = cloud
        self.accounts = accounts
        self.typeSize = typeSize
        self.apply = apply
    }

    /// The account every state assumes unless it is about not having one.
    public static let subscribed = AccountEntry(
        account: ScriptedCloudState.ada,
        entitlement: .active(source: .web, renews: nil))

    /// Signed in, nothing bought.
    public static let unsubscribed = AccountEntry(account: ScriptedCloudState.ada)
}
