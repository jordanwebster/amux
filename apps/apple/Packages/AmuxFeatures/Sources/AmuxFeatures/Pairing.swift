import AmuxCore
import AmuxDesign
import SwiftUI

/// What somebody did on a pairing screen. Like every other screen these decide
/// nothing and reach nothing: pairing is the one act in this app that changes
/// what a machine will let this phone do, so no view of it holds a capability
/// or writes trust — it says what happened and whoever owns the connection
/// carries it out.
public enum PairingAction: Equatable, Sendable {
    /// The code as it now stands. A complete one is a code to try.
    case digits(String)
    /// Trust the machine this attempt authenticated.
    case confirm(PendingPeer)
    /// Turn it away. Nothing is written on either side.
    case abandon(PendingPeer)
    /// Left without an attempt in flight.
    case cancel
    /// Get an account, because the machine the invitation names can only be
    /// reached through the relay and there is no relay without one.
    case signIn
    /// Buy the relay tunnel, because the machine authenticated and this
    /// account may not open one to it.
    case subscribe
}

/// A pairing invitation for a machine this phone cannot see.
///
/// An invitation carrying addresses is dialled on the network this phone is
/// already on and needs nothing else. One carrying none names a machine only
/// the relay has ever seen, and there is no relay without an account — so this
/// is not a refusal and not a failure of the invitation: it is the one missing
/// piece, said plainly, with the invitation kept where it was.
public struct PairNeedsAnAccount: View {
    @Environment(\.design) private var design
    private let actions: @MainActor (PairingAction) -> Void

    public init(actions: @escaping @MainActor (PairingAction) -> Void) {
        self.actions = actions
    }

    public var body: some View {
        ZStack {
            Ground()
            VStack(alignment: .leading, spacing: 0) {
                Text("That host is not on this network")
                    .designFont(.screenTitle, design)
                    .foregroundStyle(design.ink.color)
                    .fixedSize(horizontal: false, vertical: true)
                    .padding(.top, 26)
                    .identified(
                        "pair-needs-account.title",
                        value: "That host is not on this network")
                Explain("""
                    Its invitation names no address here, so it can only be reached through \
                    the relay. Sign in and this phone can take it up.
                    """)
                    .padding(.top, 8)
                Spacer(minLength: 22)
                VStack(spacing: 10) {
                    Button { actions(.signIn) } label: {
                        ActionLabel("Sign In", kind: .primary, fill: true)
                    }
                    .buttonStyle(.amuxControl)
                    .identified("pair-needs-account.signIn", label: "Sign In")
                    Button { actions(.cancel) } label: {
                        ActionLabel("Not Now", kind: .outline, fill: true)
                    }
                    .buttonStyle(.amuxControl)
                    .identified("pair-needs-account.cancel", label: "Not Now")
                }
                Explain("The invitation is kept until then.")
                    .padding(.top, 12)
                    .padding(.bottom, 34)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
            .padding(.horizontal, design.metrics.gutter)
        }
        .accessibilityElement(children: .contain)
        .identified("pair-needs-account", value: "not on this network")
    }
}

/// Adding a machine by the six-digit code it printed.
///
/// The keypad is the app's own rather than the system's. A screen whose whole
/// content is six digits has no text to edit, no selection and no other field
/// to move to, and the system's number pad brings a keyboard's worth of
/// behaviour — an accessory bar, a dictation key, a caret — for none of it. It
/// also means the digits and the keys they came from are one picture, so the
/// screen can be photographed and driven without a keyboard being up.
public struct PairByCode: View {
    @Environment(\.design) private var design
    private let model: PairingStore
    private let actions: @MainActor (PairingAction) -> Void

    public init(model: PairingStore, actions: @escaping @MainActor (PairingAction) -> Void) {
        self.model = model
        self.actions = actions
    }

    public var body: some View {
        ZStack {
            Ground()
            VStack(alignment: .leading, spacing: 0) {
                back
                heading
                boxes
                caption
                Spacer(minLength: 12)
                Keypad(enabled: taking, type: type, backspace: backspace)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
            .padding(.horizontal, design.metrics.gutter)
        }
        // A screen is a container of the things on it, not a name for all of
        // them. Without this the system spreads this identifier over every
        // element underneath — the title, the buttons, the rows — so
        // everything on the screen answers to the screen's own name, for
        // VoiceOver and for anything driving the app alike.
        .accessibilityElement(children: .contain)
        .identified("pin", value: model.digits)
    }

    /// Digits are only taken while nothing is with the machine. A code sent
    /// twice is a code spent twice, and the machine counts attempts.
    private var taking: Bool { model.taking }

    private var back: some View {
        HStack {
            BackLink("Hosts", identifier: "pin.back") { actions(.cancel) }
            Spacer()
        }
        .padding(.vertical, 8)
    }

    private var heading: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("Enter the code")
                .designFont(.screenTitle, design)
                .foregroundStyle(design.ink.color)
                .identified("pin.title", value: "Enter the code")
            Text(instruction)
                .designFont(.body, design)
                .foregroundStyle(design.inkMuted.color)
                .fixedSize(horizontal: false, vertical: true)
                .identified("pin.instruction", value: instruction)
            if let route {
                Text(route)
                    .designFont(.monoSmall, design)
                    .foregroundStyle(design.inkFaint.color)
                    .identified("pin.route", value: route)
            }
        }
        .padding(.top, 8)
        .padding(.bottom, 22)
    }

    /// Which machine to run `amux pair` on.
    ///
    /// Named, where the reference said only "the host". A six-digit code proves
    /// possession of one machine's offer and is authenticated against that
    /// machine alone, so a person typing one into a phone that knows which is
    /// owed the name — otherwise a code typed for the laptop and refused by the
    /// desktop looks like a wrong code.
    private var instruction: String {
        guard let machine = model.machine else { return "Run amux pair on the host to get one." }
        return "Run amux pair on \(machine.name) to get one."
    }

    /// Where the machine this code is for was found.
    ///
    /// Said because it is the difference between a code that will work with no
    /// account at all and one that is being carried across the relay: two
    /// machines with the same name on two different routes are otherwise one
    /// name on this screen. Nothing where no machine has been pointed at,
    /// which is a code typed before anything was found.
    private var route: String? {
        switch model.machine?.via {
        case .direct: "On this network"
        case .relay: "Through the relay"
        case .ssh: "Over SSH"
        case .offline, nil: nil
        }
    }

    private var boxes: some View {
        HStack(spacing: 8) {
            ForEach(0..<PairingStore.codeLength, id: \.self) { index in
                box(index)
            }
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("Code")
        .accessibilityValue(spokenDigits)
        .identified("pin.code", label: "Code", value: model.digits)
    }

    private func box(_ index: Int) -> some View {
        let digit = model.digits.count > index
            ? String(Array(model.digits)[index]) : ""
        // The box the next digit lands in is outlined. Nothing blinks: a caret
        // would be a promise that a keyboard is coming, and none is.
        let next = index == model.digits.count && taking
        return Text(digit)
            .designFont(.mono, design)
            .foregroundStyle(design.ink.color)
            .frame(maxWidth: .infinity)
            .frame(height: 58)
            .background {
                let shape = RoundedRectangle(
                    cornerRadius: design.metrics.controlRadius, style: .continuous)
                ZStack {
                    shape.fill(design.raised.color)
                    shape.strokeBorder(
                        next ? design.ink.color : design.hairline.color,
                        lineWidth: next ? 1.5 : 1)
                }
            }
    }

    /// The code read out as digits rather than as a number: "419" is four one
    /// nine, not four hundred and nineteen.
    private var spokenDigits: String {
        model.digits.isEmpty ? "empty" : model.digits.map(String.init).joined(separator: " ")
    }

    /// Under the boxes: when the offer runs out, or the one thing said about a
    /// code that did not work.
    @ViewBuilder
    private var caption: some View {
        switch model.phase {
        // The code was right. What is missing is the route: the machine is on
        // the far side of the relay and this account may not open a tunnel to
        // it, which no number typed here can change. So the offer stands where
        // the expiry would, and the keypad above it goes quiet.
        case .needsSubscription:
            SubscribeCallToAction(
                host: model.machine?.name, identifier: "pin.subscribe"
            ) { actions(.subscribe) }
                .padding(.top, 14)
        case .refused:
            // One sentence for a mistyped code, an expired code, a code
            // already used and a code nobody issued. Distinguishing them is
            // exactly what somebody guessing codes would want, so the screen
            // does not, and the digits have already gone.
            HStack(spacing: 8) {
                Text("That code did not work. Get a new code from the host.")
                    .designFont(.monoSmall, design)
                    .foregroundStyle(design.ink.color)
                    .fixedSize(horizontal: false, vertical: true)
                Spacer(minLength: 0)
            }
            .padding(.top, 10)
            .identified(
                "pin.refused",
                value: "That code did not work. Get a new code from the host.")
        case .checking:
            Text("Checking…")
                .designFont(.monoSmall, design)
                .foregroundStyle(design.inkMuted.color)
                .padding(.top, 10)
                .identified("pin.checking", value: "Checking…")
        default:
            Text("The code expires \(PairingStore.offerWindow) after the host prints it.")
                .designFont(.monoSmall, design)
                .foregroundStyle(design.inkFaint.color)
                .fixedSize(horizontal: false, vertical: true)
                .padding(.top, 10)
                .identified("pin.expiry", value: PairingStore.offerWindow)
        }
    }

    private func type(_ digit: Int) {
        guard taking, model.digits.count < PairingStore.codeLength else { return }
        actions(.digits(model.digits + String(digit)))
    }

    private func backspace() {
        guard taking, !model.digits.isEmpty else { return }
        actions(.digits(String(model.digits.dropLast())))
    }
}

/// Ten keys and a delete, in the arrangement every phone puts digits in.
private struct Keypad: View {
    @Environment(\.design) private var design
    let enabled: Bool
    let type: (Int) -> Void
    let backspace: () -> Void

    // The keys are one piece of glass with holes in it, not ten pieces sitting
    // near each other. Ten points apart, each plate's shadow falls across its
    // neighbours, and when every plate is its own surface the render server
    // composites those overlaps in an order it does not repeat: one key came
    // back with a heavier halo than the run before, over the whole ring around
    // it, on a screen where nothing had changed. Declaring the group is what
    // settles it, and it is also what the keypad was drawn as — the design's
    // own picture has one even shadow under each key rather than the doubled
    // edge that stacking produced. No spacing, because these keys never join.
    var body: some View {
        GlassEffectContainer(spacing: 0) {
            VStack(spacing: 10) {
                ForEach([[1, 2, 3], [4, 5, 6], [7, 8, 9]], id: \.self) { row in
                    HStack(spacing: 10) { ForEach(row, id: \.self) { key($0) } }
                }
                HStack(spacing: 10) {
                    Color.clear.frame(maxWidth: .infinity).frame(height: 52)
                    key(0)
                    Button(action: backspace) {
                        Image(systemName: "delete.backward")
                            .font(.system(size: 22, weight: .regular))
                            .foregroundStyle(enabled ? design.ink.color : design.inkFaint.color)
                            .frame(maxWidth: .infinity)
                            .frame(height: 52)
                            .contentShape(Rectangle())
                    }
                    .buttonStyle(.amuxControl)
                    .disabled(!enabled)
                    .accessibilityLabel("Delete")
                    .identified("pin.delete", label: "Delete", enabled: enabled)
                }
            }
        }
        .padding(.bottom, 12)
    }

    // The material goes on the key rather than behind it. Put behind, as a
    // background of something clear, it is a surface of its own that the group
    // above is free to draw after the digit, and the digit disappears under
    // its own key.
    private func key(_ digit: Int) -> some View {
        Button { type(digit) } label: {
            Text("\(digit)")
                .font(.system(size: 27, weight: .regular))
                .foregroundStyle(enabled ? design.ink.color : design.inkFaint.color)
                .frame(maxWidth: .infinity)
                .frame(height: 52)
                .frosted(RoundedRectangle(cornerRadius: 14, style: .continuous))
        }
        .buttonStyle(.amuxControl)
        .disabled(!enabled)
        .accessibilityLabel("\(digit)")
        .identified("pin.key.\(digit)", label: "\(digit)", enabled: enabled)
    }
}

/// The machine that answered, and the decision nobody but a person can make.
///
/// This is the whole point of pairing being two acts rather than one. Reaching
/// this screen means a secret authenticated — over a link or over a typed code
/// — and it means nothing else: no trust has been written on this phone or on
/// the machine, and leaving now writes none. What is on offer is the machine's
/// own name and the fingerprint of the key this phone would be trusting, so the
/// person can check both against what the machine itself printed.
public struct PairConfirmation: View {
    @Environment(\.design) private var design
    private let model: PairingStore
    private let actions: @MainActor (PairingAction) -> Void

    public init(model: PairingStore, actions: @escaping @MainActor (PairingAction) -> Void) {
        self.model = model
        self.actions = actions
    }

    public var body: some View {
        ZStack {
            Ground()
            VStack(alignment: .leading, spacing: 0) {
                switch model.phase {
                case .confirming(let peer): offer(peer)
                case .trusted(let name): settled(name)
                case .refused: refused
                case .needsSubscription: needsSubscription
                default: checking
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
            .padding(.horizontal, design.metrics.gutter)
        }
        // A screen is a container of the things on it, not a name for all of
        // them. Without this the system spreads this identifier over every
        // element underneath — the title, the buttons, the rows — so
        // everything on the screen answers to the screen's own name, for
        // VoiceOver and for anything driving the app alike.
        .accessibilityElement(children: .contain)
        .identified("pair-confirm", value: state)
    }

    private var state: String {
        switch model.phase {
        case .confirming(let peer): peer.name
        case .trusted(let name): "trusted \(name)"
        case .refused: "refused"
        case .needsSubscription: "needs subscription"
        default: "checking"
        }
    }

    /// The invitation was good and the machine is only on the far side of the
    /// relay. Nothing about the invitation can be retried into working, so the
    /// screen offers the one thing that would.
    private var needsSubscription: some View {
        VStack(alignment: .leading, spacing: 14) {
            SubscribeCallToAction(
                host: nil, identifier: "pair-confirm.subscribe"
            ) { actions(.subscribe) }
            Button { actions(.cancel) } label: {
                ActionLabel("Not Now", kind: .outline, fill: true)
            }
            .buttonStyle(.amuxControl)
            .identified("pair-confirm.notNow", label: "Not Now")
        }
        .padding(.top, 26)
    }

    private func offer(_ peer: PendingPeer) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            Text("Pair with \(peer.name)?")
                .designFont(.screenTitle, design)
                .foregroundStyle(design.ink.color)
                .padding(.top, 26)
                .identified("pair-confirm.title", value: "Pair with \(peer.name)?")
            Text("Match this fingerprint with the one printed by the host.")
                .designFont(.body, design)
                .foregroundStyle(design.inkMuted.color)
                .fixedSize(horizontal: false, vertical: true)
                .padding(.top, 6)
                .padding(.bottom, 22)
            VStack(alignment: .leading, spacing: 12) {
                field("Host", peer.name, mono: false, id: "name")
                // The fingerprint is the reason this screen exists: it is what
                // the person compares against the machine's own screen, and it
                // is drawn whole rather than shortened, because a fingerprint
                // with its middle taken out is not one you can check.
                field(
                "Fingerprint", Fingerprint.grouped(peer.fingerprint), mono: true,
                id: "fingerprint")
            }
            .padding(14)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background {
                RoundedRectangle(cornerRadius: design.metrics.cardRadius, style: .continuous)
                    .fill(design.raised.color)
            }
            Explain("This invitation expires \(expiry(peer)).")
                .padding(.top, 12)
                .identified("pair-confirm.expiry", value: expiry(peer))
            Spacer(minLength: 22)
            VStack(spacing: 10) {
                Button { actions(.confirm(peer)) } label: {
                    ActionLabel("Pair", kind: .primary, fill: true)
                }
                .buttonStyle(.amuxControl)
                .identified("pair-confirm.trust", label: "Pair")
                Button { actions(.abandon(peer)) } label: {
                    ActionLabel("Not Now", kind: .outline, fill: true)
                }
                .buttonStyle(.amuxControl)
                .identified("pair-confirm.abandon", label: "Not Now")
            }
            .padding(.bottom, 34)
        }
    }

    /// A labelled fact about the machine, one above the other so a long
    /// fingerprint keeps its own line rather than being squeezed beside a name.
    private func field(_ title: String, _ value: String, mono: Bool, id: String) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            Text(title.uppercased())
                .designFont(.sectionTitle, design)
                .foregroundStyle(design.inkFaint.color)
            Text(value)
                .designFont(mono ? .mono : .identifier, design)
                .foregroundStyle(design.ink.color)
                .fixedSize(horizontal: false, vertical: true)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityElement(children: .combine)
        .identified("pair-confirm.\(id)", label: title, value: value)
    }

    /// While the invitation is being authenticated. A link opens straight onto
    /// this, and it is not a screen that pairs: it is the wait before the
    /// machine has said who it is.
    private var checking: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("Checking the invitation…")
                .designFont(.bodyEmphasis, design)
                .foregroundStyle(design.ink.color)
            Explain("Nothing is trusted yet.")
        }
        .padding(.top, 26)
        .identified("pair-confirm.checking", value: "Checking the invitation…")
    }

    private var refused: some View {
        VStack(alignment: .leading, spacing: 10) {
            // The name goes on the sentence rather than on everything under
            // it: an identifier put on this stack would be spread over the
            // button as well, which is the one thing here anybody presses.
            Text("That invitation did not work")
                .designFont(.bodyEmphasis, design)
                .foregroundStyle(design.ink.color)
                .identified("pair-confirm.refused", value: "That invitation did not work")
            Explain("Get a new invitation from the host.")
            Button { actions(.cancel) } label: {
                ActionLabel("Back to Hosts", kind: .outline)
            }
            .buttonStyle(.amuxControl)
            .identified("pair-confirm.back", label: "Back to Hosts")
        }
        .padding(.top, 26)
    }

    private func settled(_ name: String) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            // The name goes on the sentence rather than on everything under
            // it: an identifier put on this stack would be spread over the
            // button as well, which is the one thing here anybody presses.
            Text("\(name) is paired")
                .designFont(.bodyEmphasis, design)
                .foregroundStyle(design.ink.color)
                .identified("pair-confirm.trusted", value: name)
            Explain("Its agents are on the Agents tab.")
            Button { actions(.cancel) } label: {
                ActionLabel("Done", kind: .outline)
            }
            .buttonStyle(.amuxControl)
            .identified("pair-confirm.done", label: "Done")
        }
        .padding(.top, 26)
    }

    /// When the offer runs out, as a length of time rather than a clock face:
    /// the machine started it, so what matters is how long is left.
    private func expiry(_ peer: PendingPeer) -> String {
        let left = peer.expiresAt.timeIntervalSince(model.now)
        return left > 0 ? "in \(Elapsed.spelled(left))" : "shortly"
    }
}
