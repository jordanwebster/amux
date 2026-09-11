import AmuxCore
import AmuxDesign
import SwiftUI

/// What somebody did on the sign-in screen.
public enum SignInAction: Equatable, Sendable {
    /// Left without signing in.
    case cancel
    /// Hand off to the account service.
    case start
    /// Signed in, and finished reading about it.
    case done
}

/// Signing in, which happens somewhere else.
///
/// The screen owns almost nothing, and that is the design: there is no field
/// here, no password, no address, no "forgot" link, because none of that is
/// this app's to hold. What it does own is the one thing a hand-off must get
/// right — naming where it is sending you. A password is about to be typed,
/// and a hand-off that does not say where it points is indistinguishable from
/// one that is lying, so the host is on the button, in the caption under it,
/// and in the sentence the screen leads with.
public struct SignIn: View {
    @Environment(\.design) private var design
    private let model: SignInStore
    private let back: String
    private let actions: @MainActor (SignInAction) -> Void

    public init(
        model: SignInStore,
        back: String = "Agents",
        actions: @escaping @MainActor (SignInAction) -> Void
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
                facts
                Spacer(minLength: 22)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
            .padding(.horizontal, design.metrics.gutter)
            foot
        }
        // A screen is a container of the things on it, not a name for all of
        // them. Without this the system spreads this identifier over every
        // element underneath, and everything on the screen answers to the
        // screen's own name — for VoiceOver and for anything driving the app.
        .accessibilityElement(children: .contain)
        .identified("sign-in", value: state)
    }

    private var state: String {
        switch model.phase {
        case .ready: "ready"
        case .handingOff: "handing off"
        case .failed: "failed"
        case .signedIn(let account): account.email
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
            .identified("sign-in.back", label: "Back to \(back)")
            .reclaimingThumbTarget(y: 13)
            Spacer()
        }
        .padding(.vertical, 8)
    }

    private var heading: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("Sign in")
                .designFont(.screenTitle, design)
                .foregroundStyle(design.ink.color)
                .identified("sign-in.title", value: "Sign in")
            Text(explanation)
                .designFont(.body, design)
                .foregroundStyle(design.inkMuted.color)
                .fixedSize(horizontal: false, vertical: true)
                .identified("sign-in.explanation", value: explanation)
        }
        .padding(.top, 8)
        .padding(.bottom, 26)
    }

    /// What an account is for, in the one sentence the reference gives it.
    ///
    /// It says nothing about the local network, because there is no local
    /// discovery: pairing itself goes over the relay, which is what the
    /// account buys. If discovery ever arrives, this is the line that changes.
    private var explanation: String {
        "An account pairs your hosts and reaches them from anywhere."
    }

    /// The two things worth knowing before pressing the button: where the
    /// signing-in happens, and that a subscription bought on the web is not
    /// bought again here.
    private var facts: some View {
        VStack(spacing: 0) {
            fact("arrow.up.forward.square", "Signed in on \(model.host)", id: "where")
            Rectangle()
                .fill(design.hairline.color)
                .frame(height: design.metrics.hairline)
                .padding(.leading, 40)
            fact("creditcard", "Web subscriptions carry over", id: "billing")
        }
    }

    private func fact(_ glyph: String, _ text: String, id: String) -> some View {
        HStack(spacing: 14) {
            Image(systemName: glyph)
                .font(.system(size: 17, weight: .regular))
                .foregroundStyle(design.inkMuted.color)
                .frame(width: 26, alignment: .leading)
            Text(text)
                .designFont(.body, design)
                .foregroundStyle(design.ink.color)
                .fixedSize(horizontal: false, vertical: true)
            Spacer(minLength: 0)
        }
        .padding(.vertical, 14)
        .accessibilityElement(children: .combine)
        .identified("sign-in.\(id)", value: text)
    }

    /// The hand-off, and everything said about it.
    ///
    /// A refusal is put here rather than at the top of the screen because this
    /// is where the person's hand already is: what went wrong and the button
    /// that tries again are one block, and neither has to be hunted for.
    private var foot: some View {
        VStack {
            Spacer(minLength: 0)
            VStack(spacing: 10) {
                trouble
                Button { actions(model.finished ? .done : .start) } label: {
                    ActionLabel(title, kind: .primary, fill: true)
                }
                .buttonStyle(.plain)
                .disabled(model.working)
                .opacity(model.working ? 0.5 : 1)
                // On the button, not on the bar it sits in: a name given to
                // the bar covers everything from here to the top of the page,
                // and a finger aimed at the middle of what that name covers
                // lands nowhere near the one thing anybody presses.
                .identified(
                    "sign-in.continue", label: title, value: state, enabled: !model.working)
                caption
            }
            .padding(14)
            .frame(maxWidth: .infinity)
            .frosted(RoundedRectangle(
                cornerRadius: design.metrics.floatRadius, style: .continuous))
            .padding(.horizontal, design.metrics.gutter)
            .padding(.bottom, 8)
        }
    }

    private var title: String {
        switch model.phase {
        case .ready: "Continue on \(model.host)"
        case .handingOff: "Opening \(model.host)…"
        case .failed: "Try Again"
        case .signedIn: "Done"
        }
    }

    /// What came back, when it was not an account.
    @ViewBuilder
    private var trouble: some View {
        if case .failed(let reason) = model.phase {
            VStack(alignment: .leading, spacing: 4) {
                Text("Sign-in did not finish")
                    .designFont(.bodyEmphasis, design)
                    .foregroundStyle(design.ink.color)
                // The cloud's own words. This app does not know what went
                // wrong on amux.sh and inventing a friendlier sentence would
                // be guessing on somebody else's behalf.
                Explain(reason)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .accessibilityElement(children: .combine)
            .identified("sign-in.failed", value: reason)
        }
    }

    /// Under the button: where it opens, and that the browser is the system's
    /// rather than a window this app draws.
    @ViewBuilder
    private var caption: some View {
        if case .signedIn(let account) = model.phase {
            Text("Signed in as \(account.email)")
                .designFont(.monoSmall, design)
                .foregroundStyle(design.inkMuted.color)
                .identified("sign-in.signed-in", value: account.email)
        } else {
            HStack(spacing: 5) {
                Image(systemName: "lock.fill")
                    .font(.system(size: 11, weight: .semibold))
                Text("Opens \(model.host) in Safari")
                    .designFont(.monoSmall, design)
            }
            .foregroundStyle(design.inkFaint.color)
            .accessibilityElement(children: .combine)
            .identified("sign-in.opens", value: model.host)
        }
    }
}

extension SignInStore {
    /// Whether the one thing left to do is leave.
    var finished: Bool {
        if case .signedIn = phase { return true }
        return false
    }
}
