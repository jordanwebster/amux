import AmuxCore
import AmuxDesign
import Foundation
import SwiftUI

/// What the plus offers.
public enum AttachChoice: String, Equatable, Sendable {
    case photo
    case file
}

/// The composer: the activity line while the agent works, the draft's
/// attachments, the field, and one round button that sends, stops, or on an
/// exited agent resumes it with the draft. Drafting never waits; sending
/// waits for the rows to be current and the agent live.
struct ComposerBox: View {
    @Environment(\.design) private var design
    @Bindable var model: ChatModel
    let placeholder: String
    let activity: Activity?
    let activitySubject: String?
    let attach: (AttachChoice) -> Void
    var focused: FocusState<Bool>.Binding

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            if let activity {
                ActivityLine(activity: activity, subject: activitySubject)
            }
            if !model.attachments.isEmpty || model.uploading > 0 { attachments }
            TextField(placeholder, text: $model.draft, axis: .vertical)
                .lineLimit(1...8)
                .designFont(.body, design)
                .foregroundStyle(design.ink.color)
                .autocorrectionDisabled()
                .textInputAutocapitalization(.sentences)
                .focused(focused)
                .identified("chat.field", label: placeholder, value: model.draft)
            HStack(spacing: 10) {
                Menu {
                    Button { attach(.photo) } label: {
                        Label(String(localized: "Photo"), systemImage: "photo")
                    }
                    Button { attach(.file) } label: {
                        Label(String(localized: "File"), systemImage: "doc")
                    }
                } label: {
                    Image(systemName: "plus")
                        .font(.system(size: 16, weight: .semibold))
                        .foregroundStyle(design.inkMuted.color)
                        .thumbTarget(x: 10, y: 10)
                }
                .accessibilityLabel("Attach")
                .identified("chat.attach", label: "Attach")
                .reclaimingThumbTarget(x: 10, y: 10)
                if !model.draft.isEmpty || !model.attachments.isEmpty {
                    Button { model.clearDraft() } label: {
                        Image(systemName: "xmark.circle.fill")
                            .font(.system(size: 15))
                            .foregroundStyle(design.inkFaint.color)
                            .thumbTarget(x: 10, y: 10)
                    }
                    .buttonStyle(.amuxControl)
                    .accessibilityLabel("Clear")
                    .identified("chat.clear", label: "Clear")
                    .reclaimingThumbTarget(x: 10, y: 10)
                }
                if let strip = model.strip, let chip = ChatWords.model(strip) {
                    Text(chip)
                        .designFont(.monoSmall, design)
                        .foregroundStyle(design.inkFaint.color)
                        .lineLimit(1)
                        .identified("chat.model", label: chip)
                }
                Spacer(minLength: 4)
                primary
            }
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 11)
        .frosted(RoundedRectangle(cornerRadius: design.metrics.floatRadius, style: .continuous))
    }

    private var attachments: some View {
        ScrollView(.horizontal) {
            HStack(spacing: 6) {
                ForEach(Array(model.attachments.enumerated()), id: \.offset) { index, attached in
                    AttachmentChip(view: attached.view, bytes: model.bytes(of:)) {
                        model.removeAttachment(at: index)
                    }
                }
                if model.uploading > 0 {
                    Text("Attaching…")
                        .designFont(.caption, design)
                        .foregroundStyle(design.inkFaint.color)
                        .identified("chat.attaching")
                }
            }
        }
        .scrollIndicators(.hidden)
    }

    private var working: Bool {
        if case .working? = model.frame?.phase { return true }
        return false
    }

    @ViewBuilder
    private var primary: some View {
        if model.frame?.composer.mode == .resume {
            Button { model.resume() } label: {
                Text("Resume")
                    .designFont(.bodyEmphasis, design)
                    .foregroundStyle(design.ground.color)
                    .padding(.horizontal, 14)
                    .frame(height: 34)
                    .background(Capsule().fill(design.ink.color))
                    .opacity(model.canResume && model.hasDraft ? 1 : 0.4)
            }
            .buttonStyle(.amuxControl)
            .disabled(!(model.canResume && model.hasDraft))
            .identified("chat.resume", label: "Resume", enabled: model.canResume && model.hasDraft)
        } else if !model.hasDraft && working {
            round("stop.fill", label: String(localized: "Stop"), id: "chat.stop", enabled: true) {
                model.interrupt()
            }
        } else {
            round(
                "arrow.up",
                label: working ? String(localized: "Queue") : String(localized: "Send"),
                id: "chat.send", enabled: model.canSend
            ) {
                model.send()
            }
        }
    }

    private func round(
        _ glyph: String, label: String, id: String, enabled: Bool, action: @escaping () -> Void
    ) -> some View {
        Button(action: action) {
            Image(systemName: glyph)
                .font(.system(size: 14, weight: .bold))
                .foregroundStyle(enabled ? design.ground.color : design.inkFaint.color)
                .frame(width: 34, height: 34)
                .background(Circle().fill(enabled ? design.ink.color : design.sunken.color))
        }
        .buttonStyle(.amuxControl)
        .disabled(!enabled)
        .accessibilityLabel(label)
        .identified(id, label: label, enabled: enabled)
    }
}

extension DraftAttachment {
    var view: AttachmentView {
        switch self {
        case .image(let blob): .image(blob)
        case .file(let blob): .file(blob)
        case .text(let name, let text): .text(name: name, lines: UInt32(text.split(separator: "\n").count))
        case .review(let diff, let comments): .review(comments: UInt32(comments.count), patch: diff.patch)
        }
    }
}

/// "Running cargo test · 12s", with the moving bar. It is not a row: it
/// says what is happening now and goes when it stops.
struct ActivityLine: View {
    @Environment(\.design) private var design
    @Environment(\.photographed) private var photographed
    @Environment(\.reducesMotion) private var reducesMotion
    let activity: Activity
    let subject: String?

    var body: some View {
        if photographed {
            line(elapsed: activity.elapsedMs, phase: 0.5)
        } else {
            TimelineView(.periodic(from: .now, by: 0.5)) { context in
                let now = Int64(context.date.timeIntervalSince1970 * 1_000)
                let elapsed = activity.sinceMs > 0 ? max(0, now - activity.sinceMs) : activity.elapsedMs
                line(
                    elapsed: elapsed,
                    phase: reducesMotion ? 0.5 : context.date.timeIntervalSince1970.truncatingRemainder(dividingBy: 1.2) / 1.2)
            }
        }
    }

    private func line(elapsed: Int64, phase: Double) -> some View {
        let words = ChatWords.activity(activity.kind, elapsedMs: elapsed, subject: subject)
        return HStack(spacing: 8) {
            GeometryReader { geometry in
                Capsule().fill(design.hairline.color)
                    .overlay(alignment: .leading) {
                        Capsule().fill(design.accent.color)
                            .frame(width: geometry.size.width * 0.35)
                            .offset(x: geometry.size.width * 0.65 * phase)
                    }
            }
            .frame(width: 26, height: 3)
            Text(words)
                .designFont(.detail, design)
                .foregroundStyle(design.inkMuted.color)
                .lineLimit(1)
                .truncationMode(.middle)
        }
        .identified("chat.activity", label: ChatWords.activity(activity.kind, elapsedMs: 0, subject: subject))
    }
}

/// Queued prompts and this phone's prompts not yet in the chat, under the
/// rows and above the composer.
struct ChatTray: View {
    @Environment(\.design) private var design
    let model: ChatModel

    var body: some View {
        let queue = model.frame?.queue ?? []
        let outbox = model.frame?.outbox ?? []
        if !queue.isEmpty || !outbox.isEmpty {
            VStack(alignment: .leading, spacing: 8) {
                ForEach(Array(queue.enumerated()), id: \.offset) { index, row in
                    trayRow(
                        text: ChatWords.text(of: row.text), state: ChatWords.queued(row),
                        warn: false, id: "chat.queued.\(index)"
                    ) {
                        if row.canSendNow {
                            small(String(localized: "Send now"), id: "chat.queued.\(index).sendNow") {
                                model.sendNow(row)
                            }
                        }
                        if row.canWithdraw {
                            small(String(localized: "Withdraw"), id: "chat.queued.\(index).withdraw") {
                                model.withdraw(row)
                            }
                        }
                    }
                }
                ForEach(Array(outbox.enumerated()), id: \.offset) { index, row in
                    trayRow(
                        text: ChatWords.text(of: row.text), state: ChatWords.outbox(row.state),
                        warn: row.state != .sending, id: "chat.outbox.\(index)"
                    ) {
                        switch row.state {
                        case .sending:
                            EmptyView()
                        case .notConfirmed:
                            small(String(localized: "Resend"), id: "chat.outbox.\(index).resend") {
                                model.resend(row.inputId)
                            }
                            small(String(localized: "Discard"), id: "chat.outbox.\(index).discard") {
                                model.discard(row.inputId)
                            }
                        case .rejected:
                            small(String(localized: "Edit"), id: "chat.outbox.\(index).edit") {
                                model.edit(row)
                            }
                            small(String(localized: "Discard"), id: "chat.outbox.\(index).discard") {
                                model.discard(row.inputId)
                            }
                        }
                    }
                }
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 10)
            .frosted(RoundedRectangle(cornerRadius: design.metrics.floatRadius, style: .continuous))
        }
    }

    private func trayRow<Actions: View>(
        text: String, state: String, warn: Bool, id: String,
        @ViewBuilder actions: () -> Actions
    ) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text(text)
                    .designFont(.detail, design)
                    .foregroundStyle(design.ink.color)
                    .lineLimit(2)
                Spacer(minLength: 6)
                Text(state)
                    .designFont(.caption, design)
                    .foregroundStyle(warn ? design.accent.color : design.inkFaint.color)
                    .lineLimit(1)
                    .fixedSize()
            }
            HStack(spacing: 14) {
                actions()
                Spacer(minLength: 0)
            }
        }
        .accessibilityElement(children: .contain)
        .identified(id, label: text, value: state)
    }

    private func small(_ title: String, id: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Text(title)
                .designFont(.bodyEmphasis, design)
                .foregroundStyle(design.accent.color)
                .thumbTarget(x: 6, y: 10)
        }
        .buttonStyle(.amuxControl)
        .identified(id, label: title)
        .reclaimingThumbTarget(x: 6, y: 10)
    }
}

/// The facts strip above the composer: tasks, context near its end, what
/// runs in the background, a usage limit coming, a tool server that failed.
/// Each part shows only while it is true.
///
/// The parts are one run of text that wraps rather than a row of labels that
/// truncate: at a phone's width three facts side by side each cut to a few
/// letters, and a fact nobody can read is not shown at all.
struct StripLine: View {
    @Environment(\.design) private var design
    let strip: Strip

    var body: some View {
        let parts = ChatWords.strip(strip)
        if !parts.isEmpty {
            HStack(spacing: 0) {
                parts.enumerated().reduce(Text(verbatim: "")) { line, item in
                    let (index, part) = item
                    let fact = Text(verbatim: part.text)
                        .foregroundStyle(part.warn ? design.accent.color : design.inkMuted.color)
                    guard index > 0 else { return line + fact }
                    return line + Text(verbatim: " · ").foregroundStyle(design.inkFaint.color) + fact
                }
                .designFont(.caption, design)
                .lineLimit(3)
                .fixedSize(horizontal: false, vertical: true)
                Spacer(minLength: 0)
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 7)
            .frosted(RoundedRectangle(cornerRadius: design.metrics.floatRadius, style: .continuous), as: .glass)
            .accessibilityElement(children: .combine)
            .identified("chat.strip", label: parts.map(\.text).joined(separator: ", "))
        }
    }
}

/// Stands in place of the composer when the agent cannot take a message
/// until something outside this chat changes: its sign-in, or its usage.
struct FootCard: View {
    @Environment(\.design) private var design
    let kind: String
    let title: String
    let detail: String

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            Image(systemName: "exclamationmark.circle")
                .foregroundStyle(design.accent.color)
            VStack(alignment: .leading, spacing: 3) {
                Text(title)
                    .designFont(.bodyEmphasis, design)
                    .foregroundStyle(design.ink.color)
                if !detail.isEmpty {
                    Text(detail)
                        .designFont(.detail, design)
                        .foregroundStyle(design.inkMuted.color)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            Spacer(minLength: 0)
        }
        .padding(16)
        .frosted(RoundedRectangle(cornerRadius: design.metrics.floatRadius, style: .continuous))
        .identified("chat.foot.\(kind)", label: title, value: detail)
    }
}
