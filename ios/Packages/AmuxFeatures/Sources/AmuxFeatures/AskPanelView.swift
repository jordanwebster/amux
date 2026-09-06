import AmuxCore
import AmuxDesign
import SwiftUI

/// What an agent is waiting to be told, where the composer would be.
///
/// An ask is a panel, not a message: a permission request is not something
/// being written, so there is no field and no footer — the answer is the whole
/// of it. It takes the composer's place rather than sitting above it, because
/// while an agent is blocked there is nothing to say to it except the answer.
///
/// Every word on it comes from the layer that asked. The command is verbatim,
/// the question's options are the agent's, the plan is the markdown it wrote,
/// and Codex's choices are the decisions Codex named — because a person
/// approving something is approving those characters and not this app's
/// summary of them.
struct AskPanelView: View {
    @Environment(\.design) private var design
    let panel: AskPanel
    let answer: @MainActor (AskDecision) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            switch panel.kind {
            case .permission(let permission):
                PermissionAsk(permission: permission, answer: answer)
            case .plan(let plan):
                PlanAsk(plan: plan, answer: answer)
            case .question(let questions):
                QuestionAsk(questions: questions, answer: answer)
            case .approval(let approval):
                ApprovalAsk(approval: approval, answer: answer)
            case .unreadable(let label):
                UnreadableAsk(label: label)
            }
        }
        .padding(14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .frosted(RoundedRectangle(cornerRadius: design.metrics.floatRadius, style: .continuous))
        .accessibilityElement(children: .contain)
        .identified("conversation.ask", value: panel.id)
    }
}

/// The line every ask opens with: the accent mark and what is wanted.
private struct AskHead: View {
    @Environment(\.design) private var design
    let glyph: String
    let title: String

    var body: some View {
        HStack(spacing: 10) {
            NeedsYouMark(glyph: glyph, size: 22)
            Text(title)
                .designFont(.bodyEmphasis, design)
                .foregroundStyle(design.ink.color)
                .fixedSize(horizontal: false, vertical: true)
            Spacer(minLength: 0)
        }
        .accessibilityElement(children: .combine)
        .identified("ask.head", label: title)
    }
}

/// What the agent wants to do, exactly as it said it.
private struct Verbatim: View {
    @Environment(\.design) private var design
    let text: String
    let literal: Bool

    var body: some View {
        Text(text)
            .designFont(literal ? .mono : .body, design)
            .foregroundStyle(design.ink.color)
            .fixedSize(horizontal: false, vertical: true)
            .padding(.horizontal, 12)
            .padding(.vertical, 10)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background {
                RoundedRectangle(cornerRadius: design.metrics.controlRadius, style: .continuous)
                    .fill(design.sunken.color)
            }
            .accessibilityLabel(text)
            .identified("ask.subject", value: text)
    }
}

// MARK: - Permission

/// Allow, deny, and the standing grant when the host offered one.
///
/// The two buttons are deliberately not the same size. Two equal buttons make
/// a fifty-fifty decision out of one that is not: the agent asked to do a
/// thing, and allowing it is what carries on. So Allow is filled and takes the
/// width it needs, and Deny is an outline beside it — the same height, the
/// same reach, and visibly the other answer.
private struct PermissionAsk: View {
    @Environment(\.design) private var design
    let permission: AskPanel.Permission
    let answer: @MainActor (AskDecision) -> Void

    var body: some View {
        AskHead(glyph: "hand.raised.fill", title: permission.headline)
        Verbatim(text: permission.subject, literal: permission.literal)
        if let purpose = permission.purpose {
            Explain(purpose)
        }
        // A panel must never offer an action the layer would refuse. Claude
        // builds its permission menu out of the host's suggestions, and this
        // build only knows how to answer the one shape it has been checked
        // against; on any other, what there is to say is where to go instead.
        if let unanswerable = permission.unanswerable {
            Explain(unanswerable)
                .identified("ask.unanswerable", label: unanswerable)
        } else {
            HStack(spacing: 10) {
                Button { answer(.allowOnce) } label: {
                    ActionLabel("Allow", kind: .primary, fill: true)
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Allow")
                .identified("ask.allow", label: "Allow")
                Button { answer(.deny(feedback: nil)) } label: {
                    ActionLabel("Deny", kind: .outline)
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Deny")
                .identified("ask.deny", label: "Deny")
            }
            if let scope = permission.scope {
                ScopeRow(scope: scope) { answer(.allowScoped(suggestion: scope.suggestion)) }
            }
        }
    }
}

/// The grant that outlives this answer.
///
/// It says what the host offered rather than what would read best. Claude
/// generates its menu from these suggestions, so a row promising to always
/// allow the command while sending a directory grant would be a lie about what
/// pressing it does.
private struct ScopeRow: View {
    @Environment(\.design) private var design
    let scope: AskPanel.Scope
    let allow: @MainActor () -> Void

    var body: some View {
        Button(action: allow) {
            HStack(spacing: 12) {
                Image(systemName: "checkmark.circle")
                    .font(.system(size: 17, weight: .regular))
                    .foregroundStyle(design.inkMuted.color)
                VStack(alignment: .leading, spacing: 1) {
                    Text(scope.title)
                        .designFont(.body, design)
                        .foregroundStyle(design.ink.color)
                    if let directory = scope.directory {
                        Text("in \(directory)")
                            .designFont(.monoSmall, design)
                            .foregroundStyle(design.inkFaint.color)
                            .lineLimit(1)
                            .truncationMode(.middle)
                    }
                }
                Spacer(minLength: 4)
                Image(systemName: "chevron.right")
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundStyle(design.inkFaint.color)
            }
            .padding(.horizontal, 13)
            .padding(.vertical, 11)
            .frame(maxWidth: .infinity, minHeight: 44, alignment: .leading)
            .background {
                RoundedRectangle(cornerRadius: design.metrics.controlRadius, style: .continuous)
                    .strokeBorder(design.hairline.color, lineWidth: 1)
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(spoken)
        .identified("ask.scope", label: spoken, value: scope.directory)
    }

    private var spoken: String {
        guard let directory = scope.directory else { return scope.title }
        return "\(scope.title) in \(directory)"
    }
}

// MARK: - Plan

/// A plan to judge, and the two things to do with it.
///
/// The plan is the agent's own markdown and is shown as such. It is capped and
/// faded rather than run out to whatever length it happens to be, because a
/// panel that fills the display stops being a panel and takes the transcript —
/// the thing that makes the plan judgeable — off the screen. The grabber opens
/// the rest in place.
private struct PlanAsk: View {
    @Environment(\.design) private var design
    let plan: AskPanel.Plan
    let answer: @MainActor (AskDecision) -> Void
    @State private var open = false
    @State private var feedback: String?

    /// How much of the display a folded plan may take. A third leaves the
    /// transcript readable behind it; opened, it takes two thirds and scrolls.
    private var cap: CGFloat { open ? 460 : 220 }

    var body: some View {
        AskHead(glyph: "list.bullet", title: "Plan")
        ScrollView {
            Prose(markdown: plan.markdown, open: false)
        }
        .scrollIndicators(.hidden)
        .scrollDisabled(!open)
        .frame(maxHeight: cap)
        .mask {
            // The fade says there is more without drawing a rule that would
            // read as the end of the plan.
            LinearGradient(
                stops: [.init(color: .black, location: 0), .init(color: .black, location: 0.86),
                        .init(color: .black.opacity(0), location: 1)],
                startPoint: .top, endPoint: .bottom)
        }
        .identified("ask.plan", value: open ? "open" : "folded")
        Button { open.toggle() } label: {
            Capsule()
                .fill(design.hairline.color)
                .frame(width: 40, height: 5)
                .frame(maxWidth: .infinity, minHeight: 22)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(open ? "Fold the plan" : "Read the whole plan")
        .identified("ask.plan.more", label: open ? "Fold the plan" : "Read the whole plan")
        HStack(spacing: 10) {
            Button { answer(.approvePlan) } label: {
                ActionLabel("Approve", kind: .primary, fill: true)
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Approve")
            .identified("ask.approve", label: "Approve")
            Button { feedback = "" } label: {
                ActionLabel("Send Back", kind: .outline)
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Send Back")
            .identified("ask.sendback", label: "Send Back")
        }
        .sheet(isPresented: Binding(get: { feedback != nil }, set: { if !$0 { feedback = nil } })) {
            // Sending a plan back without saying why is not an answer the
            // layer accepts, so what to change is asked for rather than
            // assumed.
            FeedbackSheet(
                title: "Send Back",
                prompt: "What should change?",
                send: { text in
                    feedback = nil
                    answer(.sendPlanBack(feedback: text))
                },
                cancel: { feedback = nil })
        }
    }
}

/// Free text the layer requires before it will take an answer.
struct FeedbackSheet: View {
    @Environment(\.design) private var design
    let title: String
    let prompt: String
    let send: @MainActor (String) -> Void
    let cancel: @MainActor () -> Void
    @State private var text = ""
    @FocusState private var writing: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text(title)
                .designFont(.screenTitle, design)
                .foregroundStyle(design.ink.color)
            TextField(prompt, text: $text, axis: .vertical)
                .designFont(.body, design)
                .foregroundStyle(design.ink.color)
                .lineLimit(3...8)
                .focused($writing)
                .padding(12)
                .background {
                    RoundedRectangle(
                        cornerRadius: design.metrics.controlRadius, style: .continuous)
                        .fill(design.sunken.color)
                }
                .identified("ask.feedback", value: text)
            HStack(spacing: 10) {
                Button { send(text) } label: {
                    ActionLabel("Send", kind: .primary, fill: true)
                }
                .buttonStyle(.plain)
                .disabled(text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                .identified(
                    "ask.feedback.send", label: "Send",
                    enabled: !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                Button(action: cancel) { ActionLabel("Cancel", kind: .outline) }
                    .buttonStyle(.plain)
                    .identified("ask.feedback.cancel", label: "Cancel")
            }
            Spacer(minLength: 0)
        }
        .padding(18)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background { Ground() }
        .presentationDetents([.medium])
        .onAppear { writing = true }
    }
}

// MARK: - Question

/// The agent's own question, with the agent's own answers.
///
/// A question that takes one answer is answered by tapping it: an extra
/// confirmation for a choice that is already a tap is a step that says
/// nothing. One that takes several collects them and then sends, because there
/// is no moment before the last tap at which the app could know the person is
/// finished.
private struct QuestionAsk: View {
    @Environment(\.design) private var design
    let questions: [AskPanel.Question]
    let answer: @MainActor (AskDecision) -> Void
    @State private var draft = QuestionDraft()

    /// Whether one tap is already the whole answer.
    private var immediate: Bool { QuestionDraft.immediate(questions) }

    var body: some View {
        AskHead(glyph: "questionmark", title: questions.count > 1 ? "Questions" : "Question")
        ForEach(questions) { question in
            VStack(alignment: .leading, spacing: 8) {
                if let header = question.header, questions.count > 1 {
                    Text(header.uppercased())
                        .designFont(.sectionTitle, design)
                        .foregroundStyle(design.inkFaint.color)
                }
                Text(question.prompt)
                    .designFont(.body, design)
                    .foregroundStyle(design.ink.color)
                    .fixedSize(horizontal: false, vertical: true)
                ForEach(question.options) { option in
                    OptionButton(
                        option: option,
                        picked: draft.picked(option.id, of: question.id),
                        showsPick: !immediate
                    ) {
                        pick(option.id, of: question)
                    }
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        if !immediate {
            Button { answer(.answered(draft.replies(for: questions))) } label: {
                ActionLabel("Send", kind: .primary, fill: true)
            }
            .buttonStyle(.plain)
            .disabled(!draft.isComplete(for: questions))
            .accessibilityLabel("Send")
            .identified(
                "ask.send", label: "Send", enabled: draft.isComplete(for: questions))
        }
    }

    private func pick(_ option: Int, of question: AskPanel.Question) {
        draft.toggle(option, of: question)
        // One question that takes one answer is finished the moment it is
        // tapped. Anything else is not: the layer refuses a response with a
        // question missing, so the rest are collected and sent together.
        guard immediate else { return }
        answer(.answered(draft.replies(for: questions)))
    }
}

private struct OptionButton: View {
    @Environment(\.design) private var design
    let option: AskPanel.Question.Option
    let picked: Bool
    let showsPick: Bool
    let choose: @MainActor () -> Void

    var body: some View {
        Button(action: choose) {
            HStack(spacing: 10) {
                if showsPick {
                    Image(systemName: picked ? "checkmark.circle.fill" : "circle")
                        .font(.system(size: 17, weight: .regular))
                        .foregroundStyle(picked ? design.ink.color : design.inkFaint.color)
                }
                VStack(alignment: showsPick ? .leading : .center, spacing: 1) {
                    Text(option.label)
                        .designFont(.bodyEmphasis, design)
                        .foregroundStyle(design.ink.color)
                    if let description = option.description {
                        Text(description)
                            .designFont(.caption, design)
                            .foregroundStyle(design.inkMuted.color)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
                .frame(maxWidth: .infinity, alignment: showsPick ? .leading : .center)
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 11)
            .frame(maxWidth: .infinity, minHeight: 44)
            .background {
                RoundedRectangle(cornerRadius: design.metrics.controlRadius, style: .continuous)
                    .fill(design.sunken.color)
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(option.label)
        .accessibilityValue(picked ? "Chosen" : "")
        .identified(
            "ask.option.\(option.id)", label: option.label, value: picked ? "chosen" : nil)
    }
}

// MARK: - Codex

/// Codex asking, in Codex's words, with Codex's decisions.
///
/// The choices are listed in the order Codex offered them and named as Codex
/// names them. A choice this build cannot carry is still shown and cannot be
/// pressed: hiding it would misrepresent what the far side offered, and
/// offering it would send something the backend refuses.
private struct ApprovalAsk: View {
    @Environment(\.design) private var design
    let approval: AskPanel.Approval
    let answer: @MainActor (AskDecision) -> Void

    var body: some View {
        AskHead(glyph: "hand.raised.fill", title: approval.headline)
        if let subject = approval.subject {
            Verbatim(text: subject, literal: true)
        }
        if let place = approval.place {
            Text(place)
                .designFont(.monoSmall, design)
                .foregroundStyle(design.inkFaint.color)
                .lineLimit(1)
                .truncationMode(.middle)
                .identified("ask.place", value: place)
        }
        if let reason = approval.reason {
            Explain(reason)
        }
        VStack(spacing: 8) {
            ForEach(approval.choices) { choice in
                Button { choice.decision.map { answer(.decided($0)) } } label: {
                    ActionLabel(
                        choice.label, kind: choice.id == 0 ? .primary : .outline, fill: true)
                }
                .buttonStyle(.plain)
                .disabled(choice.decision == nil)
                .opacity(choice.decision == nil ? 0.45 : 1)
                .accessibilityLabel(choice.label)
                .identified(
                    "ask.decision.\(choice.id)", label: choice.label,
                    enabled: choice.decision != nil)
            }
        }
    }
}

// MARK: - Unreadable

/// An ask this build has no panel for. It is stated rather than hidden: an
/// agent waiting on an answer nobody can see is a conversation that has
/// silently stopped.
private struct UnreadableAsk: View {
    @Environment(\.design) private var design
    let label: String

    var body: some View {
        AskHead(glyph: "questionmark", title: "Waiting on an answer")
        Explain("This build cannot read a \(label) ask. Attach to the session to answer it.")
            .identified("ask.unreadable", value: label)
    }
}

// MARK: - Finished

/// The turn is over and something changed.
///
/// It sits where an ask would, because a finished turn is the last thing that
/// needs you: the changes are the reason to have opened the conversation, and
/// Later is the honest other answer — nothing is sent, and the panel goes away
/// for this visit.
struct FinishedPanel: View {
    @Environment(\.design) private var design
    let changes: ReviewDocument
    let review: @MainActor () -> Void
    let later: @MainActor () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            AskHead(glyph: "checkmark", title: "Finished")
            Explain(arithmetic)
            HStack(spacing: 10) {
                Button(action: review) {
                    ActionLabel("Review Changes", kind: .primary, fill: true)
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Review Changes")
                .identified("ask.review", label: "Review Changes")
                Button(action: later) { ActionLabel("Later", kind: .outline) }
                    .buttonStyle(.plain)
                    .accessibilityLabel("Later")
                    .identified("ask.later", label: "Later")
            }
        }
        .padding(14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .frosted(RoundedRectangle(cornerRadius: design.metrics.floatRadius, style: .continuous))
        .accessibilityElement(children: .contain)
        .identified("conversation.finished", value: arithmetic)
    }

    /// "+118 −40", counted off the patch this opens rather than repeated
    /// from the fleet's totals for the last turn. A number that disagreed with
    /// the page it opens would be worse than no number.
    private var arithmetic: String {
        "+\(changes.insertions) \u{2212}\(changes.deletions)"
    }
}
