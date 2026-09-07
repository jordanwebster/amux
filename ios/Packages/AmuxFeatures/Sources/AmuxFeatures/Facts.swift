import AmuxCore
import AmuxDesign
import SwiftUI

/// What is true about the turn, on one surface directly above the composer.
///
/// One strip carrying whichever facts are true and nothing else. A row appears
/// only while its fact is true, so a quiet agent has no strip at all and
/// nothing down here is permanent chrome. It is not a card somebody opened:
/// it is a fact about the running turn, so it stays bright behind anything
/// that is open and there is nothing on it to dismiss.
///
/// Two rows are drawn today, in this order, because that is the order they are
/// read in: what is being done and how far through it is, then the message
/// waiting to go. What the agent is doing *this second* is deliberately not
/// repeated here — it is the line at the top of the composer, one plate below,
/// and the same sentence twice with the second copy truncated is the version a
/// person would try to read.
///
/// Opened, the strip grows in place. The summary line stays exactly where it
/// was and the list appears above it, so what was tapped does not move out
/// from under the thumb.
struct FactsStrip: View {
    @Environment(\.design) private var design
    let facts: ConversationFacts
    /// Whether the task list is showing.
    let open: Bool
    /// Show or hide the list. Absent from the strip when there is no list to
    /// grow into.
    let grow: @MainActor () -> Void
    /// Take the held message back into the field.
    let unqueue: @MainActor () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            if open, let tasks = facts.tasks, !tasks.items.isEmpty {
                TaskListPanel(tasks: tasks)
                Divider().overlay(design.hairline.color)
            }
            if facts.progress != nil {
                taskRow
            }
            if let queued = facts.queued {
                if facts.progress != nil { Divider().overlay(design.hairline.color) }
                queuedRow(queued)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .frosted(RoundedRectangle(cornerRadius: design.metrics.floatRadius, style: .continuous))
        .accessibilityElement(children: .contain)
        .identified("facts", value: open ? "open" : "folded")
    }

    // MARK: - What is being done

    /// The count, the task, and on the trailing edge the two things that are
    /// about the task rather than in it: how many agents this one started, and
    /// the way into the list.
    private var taskRow: some View {
        HStack(spacing: 10) {
            if let progress = facts.progress {
                Text(progress)
                    .designFont(.caption, design)
                    .foregroundStyle(design.inkMuted.color)
                    .monospacedDigit()
            }
            // Absent rather than blank when the provider named no current
            // task: a list can be all done, and an empty line where a
            // sentence belongs reads as something that failed to load.
            if let current = facts.current {
                Text(current)
                    .designFont(.body, design)
                    .foregroundStyle(design.ink.color)
                    .lineLimit(1)
                    .truncationMode(.tail)
            }
            Spacer(minLength: 6)
            if let children = facts.children {
                ChildrenChip(children: children)
            }
            if facts.opens {
                Button(action: grow) {
                    Image(systemName: open ? "chevron.down" : "chevron.up")
                        .font(.system(size: 13, weight: .semibold))
                        .foregroundStyle(design.inkMuted.color)
                        .frame(width: 32, height: 44)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityLabel(open ? "Hide tasks" : "Show tasks")
                .identified("facts.grow", label: open ? "Hide tasks" : "Show tasks")
            }
        }
        .padding(.leading, 16)
        .padding(.trailing, facts.opens ? 8 : 16)
        .frame(minHeight: 52)
        .accessibilityElement(children: .contain)
        .identified(
            "facts.task", label: [facts.progress, facts.current].compactMap { $0 }
                .joined(separator: " "),
            value: facts.progress ?? "")
    }

    // MARK: - What is waiting to go

    /// The held message, said once and elided, with the way out of the queue
    /// on the row itself.
    ///
    /// The whole row is the control. Tapping it unqueues: the text lands in
    /// the field, this row goes, and what is in front of you is an ordinary
    /// unsent message. There is no edit mode, no banner naming what is being
    /// changed, and no discard — abandoning it is clearing the field, the way
    /// every other unsent message is abandoned.
    private func queuedRow(_ queued: QueuedMessage) -> some View {
        Button(action: unqueue) {
            HStack(spacing: 10) {
                Image(systemName: "clock")
                    .font(.system(size: 14, weight: .regular))
                    .foregroundStyle(design.inkFaint.color)
                    .frame(width: 18)
                Text(queued.text)
                    .designFont(.body, design)
                    .foregroundStyle(design.inkMuted.color)
                    .lineLimit(1)
                    .truncationMode(.tail)
                Spacer(minLength: 6)
                // The pencil says the row is the way back to the field. It is
                // drawn only while the message can still be changed: one
                // already on its way cannot be, the core refuses to touch it,
                // and a control whose only outcome is a refusal is worse than
                // no control.
                if queued.changeable {
                    Image(systemName: "pencil")
                        .font(.system(size: 14, weight: .regular))
                        .foregroundStyle(design.inkFaint.color)
                }
            }
            .padding(.horizontal, 16)
            .frame(minHeight: 52)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(!queued.changeable)
        .accessibilityLabel("Queued: \(queued.text)")
        .accessibilityHint("Unqueues it back into the field")
        .identified(
            "facts.queued", label: queued.text, value: queued.spoken,
            enabled: queued.changeable)
    }
}

/// The whole list, in the provider's own order, above the line that
/// summarises it.
///
/// It takes exactly the room the list needs, up to a cap, and scrolls past
/// that. Both halves matter: a list left to grow without limit would push the
/// composer off the display, and a panel that took the cap whether or not the
/// list filled it would leave a band of empty glass under the last task, which
/// reads as tasks that failed to load. So the list is measured and the panel is
/// exactly that tall — the measurement is of the content under an unbounded
/// proposal, so it cannot chase the frame it decides.
///
/// The cap leaves the feed above the strip readable, which is the reason to
/// have one at all.
private struct TaskListPanel: View {
    let tasks: TaskList
    /// What the list actually needs, which depends on the reader's type size
    /// and on how long the provider's sentences are.
    @State private var needed: CGFloat = 0

    private static let cap: CGFloat = 380

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 0) {
                ForEach(Array(tasks.items.enumerated()), id: \.offset) { index, item in
                    TaskRow(item: item, at: index)
                }
            }
            .padding(.vertical, 6)
            .onGeometryChange(for: CGFloat.self) { $0.size.height } action: { needed = $0 }
        }
        .scrollIndicators(.hidden)
        .scrollDisabled(needed <= Self.cap)
        .frame(height: min(max(needed, 1), Self.cap))
        .identified("facts.tasks", value: "\(tasks.items.count)")
    }
}

/// How many agents this one started, and whether any of them is stuck.
///
/// A number rather than a list, because the agents themselves are named in
/// full above the feed, one chip each. What this adds is the count in the one
/// place a person is already looking while a turn runs — and the colour, when
/// one of them has stopped and cannot go on without somebody. That is the one
/// coloured thing in the strip, for the same reason it is the one coloured
/// thing on the home: the accent is this app's only word for "something is
/// waiting for you".
private struct ChildrenChip: View {
    @Environment(\.design) private var design
    let children: ConversationFacts.Children

    var body: some View {
        HStack(spacing: 5) {
            Image(systemName: "arrow.triangle.branch")
                .font(.system(size: 11, weight: .semibold))
            Text("\(children.count)")
                .designFont(.caption, design)
                .monospacedDigit()
        }
        .foregroundStyle(stuck ? design.onAccent.color : design.inkMuted.color)
        .padding(.horizontal, 9)
        .padding(.vertical, 5)
        .background {
            Capsule().fill(stuck ? design.accent.color : design.sunken.color)
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel(spoken)
        .identified("facts.children", label: spoken, value: "\(children.count)")
    }

    private var stuck: Bool { children.needs != nil }

    private var spoken: String {
        let started = children.count == 1 ? "1 agent started" : "\(children.count) agents started"
        guard let needs = children.needs else { return started }
        return "\(started), one \(needs.spoken.lowercased())"
    }
}

/// One task of the provider's list, in the grown strip.
///
/// Marked rather than coloured. Which one is being worked on is said by its
/// weight and by a filled mark, not by the accent: the accent means somebody
/// is being waited for, and an agent working through its own list is not
/// waiting for anybody.
private struct TaskRow: View {
    @Environment(\.design) private var design
    let item: TaskItem
    let at: Int

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            Image(systemName: glyph)
                .font(.system(size: 12, weight: .semibold))
                .foregroundStyle(design.inkFaint.color)
                .frame(width: 18, height: 20)
            Text(item.text)
                .designFont(item.state == .inProgress ? .bodyEmphasis : .body, design)
                .foregroundStyle(ink)
                .fixedSize(horizontal: false, vertical: true)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 7)
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(item.text), \(spoken)")
        .identified("facts.task.\(at)", label: item.text, value: spoken)
    }

    private var glyph: String {
        switch item.state {
        case .completed: "checkmark"
        case .inProgress: "circle.inset.filled"
        case .pending: "circle"
        }
    }

    /// Done work recedes and work not started yet is quieter than the line
    /// being worked on, which is the only one a reader is looking for.
    private var ink: Color {
        switch item.state {
        case .completed: design.inkFaint.color
        case .inProgress: design.ink.color
        case .pending: design.inkMuted.color
        }
    }

    private var spoken: String {
        switch item.state {
        case .completed: "done"
        case .inProgress: "in progress"
        case .pending: "not started"
        }
    }
}

extension QueuedMessage {
    /// What state the held message is in, for a reader who cannot see whether
    /// the pencil is there.
    public var spoken: String {
        switch delivery {
        case .held: "waiting for the turn to end"
        case .sending: "on its way"
        // The core's own sentence where it gave one. A refusal with no words
        // is still a refusal, and saying so beats saying nothing.
        case .failed(let failure): failure.message ?? "not sent"
        }
    }
}
