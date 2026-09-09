import AmuxCore
import AmuxDesign
import SwiftUI

/// What somebody did on the paywall.
public enum PaywallAction: Equatable, Sendable {
    case cancel
    case choose(Plan.Period)
    case buy
    case restore
    /// Offer a purchase amux.sh has not confirmed to it again.
    case retry
    /// Subscribed, and finished reading about it.
    case done
}

/// Subscribing.
///
/// One sentence about what the subscription is, then the plans. There is no
/// list of perks, because a list of perks implies a version without them and
/// there is not one: the subscription is what connects this phone to any host
/// at all. Nothing here is dressed up — no badges, no countdown, no "most
/// popular" — because the decision is whether the app works, and a screen that
/// pushed would be pushing on the only door.
public struct Paywall: View {
    @Environment(\.design) private var design
    private let model: PaywallStore
    private let back: String
    private let actions: @MainActor (PaywallAction) -> Void

    public init(
        model: PaywallStore,
        back: String = "You",
        actions: @escaping @MainActor (PaywallAction) -> Void
    ) {
        self.model = model
        self.back = back
        self.actions = actions
    }

    public var body: some View {
        ZStack {
            Ground()
            VStack(alignment: .leading, spacing: 0) {
                backButton
                heading
                if model.entitled {
                    subscribed
                } else if settling {
                    // A purchase already exists. Leaving two plans on screen
                    // to be picked would be offering to buy a second one, and
                    // the button underneath does not buy — it sends the one
                    // that was bought to amux.sh again.
                    EmptyView()
                } else {
                    plans
                    terms
                }
                Spacer(minLength: 22)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
            .padding(.horizontal, design.metrics.gutter)
            foot
        }
        // A screen is a container of the things on it, not a name for all of
        // them: without this the system spreads this identifier over the
        // title, the rows and the buttons underneath.
        .accessibilityElement(children: .contain)
        .identified("paywall", value: state)
    }

    private var state: String {
        switch model.phase {
        case .ready: model.chosen.rawValue
        case .buying: "buying"
        case .awaitingApproval: "awaiting approval"
        case .confirming: "confirming"
        case .unconfirmed(let why): "unconfirmed \(why.named)"
        case .failed: "failed"
        case .entitled(let grant): "entitled by \(grant.named)"
        }
    }

    private var backButton: some View {
        HStack {
            Button { actions(.cancel) } label: {
                HStack(spacing: 3) {
                    Image(systemName: "chevron.left")
                        .font(.system(size: 17, weight: .semibold))
                    Text(back)
                        .designFont(.body, design)
                }
                .foregroundStyle(design.accent.color)
                .thumbTarget(y: 13)
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Back to \(back)")
            .identified("paywall.back", label: "Back to \(back)")
            .reclaimingThumbTarget(y: 13)
            Spacer()
        }
        .padding(.vertical, 8)
    }

    private var heading: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("Reach your agents anywhere")
                .designFont(.screenTitle, design)
                .foregroundStyle(design.ink.color)
                .fixedSize(horizontal: false, vertical: true)
                .identified("paywall.title", value: "Reach your agents anywhere")
            Text(explanation)
                .designFont(.body, design)
                .foregroundStyle(design.inkMuted.color)
                .fixedSize(horizontal: false, vertical: true)
                .identified("paywall.explanation", value: explanation)
        }
        .padding(.top, 8)
        .padding(.bottom, 22)
    }

    private var explanation: String {
        "Subscribe to reach your hosts through the relay."
    }

    private var plans: some View {
        VStack(spacing: 0) {
            ForEach(Array(model.plans.enumerated()), id: \.element.id) { index, plan in
                row(plan)
                if index < model.plans.count - 1 {
                    Rectangle()
                        .fill(design.hairline.color)
                        .frame(height: design.metrics.hairline)
                        .padding(.leading, 40)
                }
            }
        }
    }

    private func row(_ plan: Plan) -> some View {
        let chosen = plan.period == model.chosen
        return Button { actions(.choose(plan.period)) } label: {
            HStack(spacing: 14) {
                // The mark is the system's radio, drawn rather than a check:
                // these two are alternatives, and a check would read as two
                // things that could both be on.
                Image(systemName: chosen ? "largecircle.fill.circle" : "circle")
                    .font(.system(size: 21, weight: .regular))
                    .foregroundStyle(chosen ? design.accent.color : design.inkFaint.color)
                    .frame(width: 26)
                Text(plan.period.title)
                    .designFont(.body, design)
                    .foregroundStyle(design.ink.color)
                Spacer(minLength: 8)
                VStack(alignment: .trailing, spacing: 2) {
                    Text(plan.price)
                        .designFont(.bodyEmphasis, design)
                        .foregroundStyle(design.ink.color)
                    if let saving = plan.saving {
                        Text(saving)
                            .designFont(.monoSmall, design)
                            .foregroundStyle(design.inkMuted.color)
                    }
                }
            }
            .padding(.vertical, 14)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(model.working)
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(chosen ? [.isSelected] : [])
        .identified(
            "paywall.\(plan.period.rawValue)",
            label: "\(plan.period.title) \(plan.price)",
            value: chosen ? "chosen" : "not chosen", enabled: !model.working)
    }

    /// What is true about every subscription, whichever is chosen. Where it is
    /// cancelled, and that a subscription bought anywhere else already counts.
    private var terms: some View {
        Text("Cancel any time in the App Store. A subscription bought on the web carries over.")
            .designFont(.monoSmall, design)
            .foregroundStyle(design.inkFaint.color)
            .fixedSize(horizontal: false, vertical: true)
            .padding(.top, 14)
            .identified("paywall.terms")
    }

    /// Somebody who already has it. The screen says so and where it came
    /// from, rather than offering to sell a second subscription for the same
    /// thing.
    @ViewBuilder
    private var subscribed: some View {
        if let grant = model.grant {
            VStack(alignment: .leading, spacing: 4) {
                Text(Self.subscribed(grant))
                    .designFont(.bodyEmphasis, design)
                    .foregroundStyle(design.ink.color)
                Explain(Self.honoured(grant))
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.vertical, 14)
            .accessibilityElement(children: .combine)
            .identified("paywall.subscribed", value: grant.named)
        }
    }

    /// Where the subscription was bought, in the preposition that place
    /// takes: one is a store you are in, the other a site you are on. Access
    /// that was given was bought nowhere, so it names no place and does not
    /// call itself a subscription.
    static func subscribed(_ grant: Grant) -> String {
        switch grant {
        case .purchased(.appStore): "Subscribed in the App Store"
        case .purchased(.web): "Subscribed on amux.sh"
        case .granted: "Pro is on for this account"
        }
    }

    /// A subscription bought anywhere counts everywhere, and where it is
    /// managed is where it was bought. Access that was given is managed
    /// nowhere, and saying otherwise would send somebody looking for a
    /// subscription that does not exist.
    static func honoured(_ grant: Grant) -> String {
        switch grant {
        case .purchased(.appStore):
            "Manage your subscription in the App Store."
        case .purchased(.web):
            "Manage your subscription on amux.sh."
        case .granted:
            "Relay access is included with this account."
        }
    }

    /// The one thing pressed, and everything said about pressing it.
    private var foot: some View {
        VStack {
            Spacer(minLength: 0)
            VStack(spacing: 10) {
                trouble
        Button { actions(pressing) } label: {
                    ActionLabel(title, kind: .primary, fill: true)
                }
                .buttonStyle(.plain)
                .disabled(model.working || waiting)
                .opacity(model.working || waiting ? 0.5 : 1)
                // On the button rather than on the bar it sits in: the bar is
                // pinned to the foot of a full-height stack, so a name given
                // to it covers the whole page.
                .identified(
                    "paywall.buy", label: title, value: state,
                    enabled: !model.working && !waiting)
                if !model.entitled {
                    Button { actions(.restore) } label: {
                        Text("Restore Purchases")
                            .designFont(.mono, design)
                            .foregroundStyle(design.accent.color)
                            .thumbTarget(y: 14)
                    }
                    .buttonStyle(.plain)
                    .disabled(model.working)
                    .identified(
                        "paywall.restore", label: "Restore Purchases", enabled: !model.working)
                    .reclaimingThumbTarget(y: 14)
                }
            }
            .padding(14)
            .frame(maxWidth: .infinity)
            .frosted(RoundedRectangle(
                cornerRadius: design.metrics.floatRadius, style: .continuous))
            .padding(.horizontal, design.metrics.gutter)
            .padding(.bottom, 8)
        }
    }

    /// Whether a purchase has been made and is on its way to amux.sh — posted
    /// and waiting, or waiting to be sent again.
    private var settling: Bool {
        model.phase == .confirming || unconfirmed
    }

    /// A purchase the store has taken and cannot finish. Buying again would be
    /// a second charge for the same month, so the button stops offering it.
    private var waiting: Bool { model.phase == .awaitingApproval }

    /// Whether this purchase is bought and still not this account's. The
    /// button offers it to amux.sh again rather than offering to buy a second
    /// one, which is the one thing that must not happen here.
    private var unconfirmed: Bool {
        if case .unconfirmed = model.phase { return true }
        return false
    }

    /// What pressing the one button means, which is not always buying.
    private var pressing: PaywallAction {
        if model.entitled { return .done }
        return unconfirmed ? .retry : .buy
    }

    private var title: String {
        if model.entitled { return "Done" }
        switch model.phase {
        case .buying: return "Waiting for the App Store…"
        case .awaitingApproval: return "Waiting for approval"
        case .confirming: return "Confirming with amux.sh…"
        case .unconfirmed: return "Retry"
        default: break
        }
        guard let plan = model.plan else { return "Subscribe" }
        return "Subscribe · \(plan.price) \(plan.period.spelled)"
    }

    /// What came back, when it was not a subscription.
    @ViewBuilder
    private var trouble: some View {
        switch model.phase {
        case .failed(let reason):
            note("That did not go through", reason, id: "failed", value: reason)
        case .awaitingApproval:
            // Ask to Buy, or a bank asking for a second factor. Nothing is
            // owed and nothing is bought, and the person has to be told that
            // rather than left looking at a button that stopped working.
            note(
                "Waiting for approval",
                "Your subscription starts when the App Store approves your purchase.",
                id: "pending", value: "pending")
        // Paid for, and amux.sh has not said so yet. The purchase is kept
        // either way: the App Store still holds it, so pressing Retry — or
        // just opening the app again — offers it once more.
        case .unconfirmed(let why):
            note(
                "Your purchase is not confirmed yet",
                Self.unconfirmed(why), id: "unconfirmed", value: why.named)
        default:
            EmptyView()
        }
    }

    /// Why a purchase that went through is not this account's yet, and what
    /// happens next. A phone that could not get through will try again on its
    /// own; amux.sh saying no will not change by waiting, so that one says
    /// where to go instead.
    static func unconfirmed(_ why: PaywallStore.Unconfirmed) -> String {
        switch why {
        case .unreachable:
            "Your purchase is saved, and the app retries confirmation with amux.sh when it next opens."
        case .refused(let said):
            // Ended for us, because the reason is the account service's
            // sentence and nothing makes it finish one.
            "Your purchase is saved, but amux.sh could not confirm it: \(ended(said))"
        }
    }

    /// Somebody else's words, finished. A reason that already ends in a stop
    /// keeps it; one that does not is given one, so it does not run into the
    /// sentence after it.
    static func ended(_ said: String) -> String {
        let trimmed = said.trimmingCharacters(in: .whitespaces)
        guard let last = trimmed.last, !".!?".contains(last) else { return trimmed }
        return trimmed + "."
    }

    private func note(_ headline: String, _ detail: String, id: String, value: String) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(headline)
                .designFont(.bodyEmphasis, design)
                .foregroundStyle(design.ink.color)
            Explain(detail)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityElement(children: .combine)
        // The words as well as the state, because two states can be the same
        // state and still have to say different things — a purchase amux.sh
        // could not be told about and one it refused are both unconfirmed.
        .identified("paywall.\(id)", label: detail, value: value)
    }
}
