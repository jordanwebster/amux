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
    /// Whether this state is drawn for a reader who has asked the system for
    /// less motion, and for one who has asked for less transparency. Declared
    /// rather than read off the device, because both are settings outside the
    /// app that no test can change from inside it.
    public let reduceMotion: Bool
    public let reduceTransparency: Bool
    /// What the conversation has opened over itself in this state, where the
    /// screen name does not say. Two states are the settings screen — the
    /// model sheet and the permissions sheet — so which one is a fact about
    /// the state rather than about the screen.
    public let overlay: ConversationOverlay?
    /// The report this state is in the middle of writing, if any. Declared
    /// rather than reached, for the same reason a purchase is: a report starts
    /// from a screenshot the system takes, and nothing inside the app can make
    /// one happen.
    public let report: Reporting?
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
        reduceMotion: Bool = false,
        reduceTransparency: Bool = false,
        overlay: ConversationOverlay? = nil,
        report: Reporting? = nil,
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
        self.reduceMotion = reduceMotion
        self.reduceTransparency = reduceTransparency
        self.overlay = overlay
        self.report = report
        self.apply = apply
    }

    /// A report in progress, as a state declares it: the note, the rectangles
    /// somebody drew, and where the send stands.
    public struct Reporting: Sendable, Equatable {
        public var note: String
        public var marks: [ReportMark]
        public var sending: Sending

        /// Where the send has got to. Spelled here rather than reusing the
        /// store's own phase, because a fixture declares a resting state and
        /// the store's phase carries a receipt nobody would want to write out.
        public enum Sending: Sendable, Equatable {
            case ready
            case failed(String)
        }

        public init(note: String, marks: [ReportMark] = [], sending: Sending = .ready) {
            self.note = note
            self.marks = marks
            self.sending = sending
        }
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
        entitlement: .active(grant: .purchased(.web), renews: nil))

    /// Signed in, nothing bought.
    public static let unsubscribed = AccountEntry(account: ScriptedCloudState.ada)

    /// Signed in and entitled, having bought nothing: the access was given.
    /// Every screen that reads an entitlement has something different to say
    /// about this account, and none of it may name a store.
    public static let given = [AccountEntry(
        account: ScriptedCloudState.ada,
        entitlement: .active(grant: .granted, renews: nil), hosts: 3)]

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
            entitlement: .active(grant: .purchased(.web), renews: nil), hosts: 3),
        AccountEntry(
            account: SignedInAccount(
                id: AccountId("acme"), email: "ada@acme.example", displayName: "Acme"),
            entitlement: .active(grant: .purchased(.appStore), renews: nil)),
        AccountEntry(
            account: SignedInAccount(
                id: AccountId("side"), email: "side@example.com", displayName: "Side project"),
            signedIn: false),
    ]
}

/// The picture a report fixture is frozen on.
///
/// A report screen is mostly a photograph of another screen, so a state that
/// declared one without a picture would be a baseline of a grey rectangle. The
/// picture is a real capture of the conversation, committed beside this file
/// at one pixel per point so it is a fixture rather than a copy of a golden.
public enum FrozenFixture {
    /// The conversation, as this phone would have photographed it.
    ///
    /// Nothing here reaches a window: the point of a fixture is that the state
    /// it declares exists without the events that would have produced it.
    public static func capture() -> ReportCapture? {
        // The app's own bundle, not a package's. These sources are compiled
        // into the debug app rather than linked as a package — Xcode
        // force-loads a package's static library whether or not anything
        // references it, which would put every fixture in the shipped binary
        // — so there is no package bundle to ask and the picture is an app
        // resource that release leaves out with the rest of them.
        guard
            let url = Bundle.main.url(forResource: "frozen-frame", withExtension: "png"),
            let png = try? Data(contentsOf: url)
        else { return nil }
        return ReportCapture(
            frame: FrozenFrame(png: png, width: 402, height: 874, scale: 3),
            snapshot: snapshot,
            trace: "{\"kind\":\"route\",\"screen\":\"run\"}\n",
            route: "run")
    }

    /// What the runtime would have answered: a checkpoint, one folded message
    /// and the embedded daemon's dump. Small on purpose — what a bundle test
    /// checks is that each part is declared and carried, not what is in it.
    private static let snapshot = """
        {"msgs":{"format_version":1,"checkpoint":{"agents":[]},\
        "msgs":["{\\"kind\\":\\"fleet\\"}"]},"daemon":"{\\"hosts\\":[]}"}
        """
}
