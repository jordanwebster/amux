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
    /// The side of every control's target in the row under the field.
    static let slot: CGFloat = 44
    /// The drawn send, stop and resume controls, centred in their slots.
    static let round: CGFloat = 34

    @Environment(\.design) private var design
    @Bindable var model: ChatModel
    let placeholder: String
    let activity: Activity?
    let activitySubject: String?
    let attach: (AttachChoice) -> Void
    /// Dictation's two acts: `.dictate` and `.dictationSettings`.
    let dictate: (ChatAction) -> Void
    /// Opens the settings card, from the model chip or the plus menu.
    let openSettings: () -> Void
    var focused: FocusState<Bool>.Binding

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            if let activity {
                ActivityLine(activity: activity, subject: activitySubject)
            }
            if let sentence = model.dictation.sentence { dictationLine(sentence) }
            if !model.attachments.isEmpty || model.uploading > 0 { attachments }
            TextField(placeholder, text: Binding(get: { model.draft }, set: { model.type($0) }), axis: .vertical)
                .lineLimit(1...8)
                .designFont(.body, design)
                .foregroundStyle(design.ink.color)
                .autocorrectionDisabled()
                .textInputAutocapitalization(.sentences)
                .focused(focused)
                .identified("chat.field", label: placeholder, value: model.draft)
            // Every control here is a 44 pt slot, side by side rather than grown
            // over each other: the icons are drawn closer than two thumbs are
            // wide. The row then gives back the slack around the glyphs at its
            // edges, so the plus lines up with the field and the send circle
            // sits where the padding puts it.
            HStack(spacing: 0) {
                MenuButton(
                    name: String(localized: "Attach"), identifier: "chat.attach", items: attachItems
                ) {
                    Image(systemName: "plus")
                        .font(.system(size: 16, weight: .semibold))
                        .foregroundStyle(design.inkMuted.color)
                        .frame(width: ComposerBox.slot, height: ComposerBox.slot)
                        .contentShape(Rectangle())
                }
                Button { dictate(.dictate) } label: {
                    Image(systemName: model.dictation.active ? "stop.circle.fill" : "mic")
                        .font(.system(size: 16, weight: .semibold))
                        .foregroundStyle(model.dictation.active ? design.accent.color : design.inkMuted.color)
                        .frame(width: ComposerBox.slot, height: ComposerBox.slot)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.amuxControl)
                .accessibilityLabel(dictationLabel)
                .identified("chat.dictate", label: dictationLabel,
                            value: model.dictation.active ? "listening" : "idle")
                if !model.draft.isEmpty || !model.attachments.isEmpty {
                    Button { model.clearDraft() } label: {
                        Image(systemName: "xmark.circle.fill")
                            .font(.system(size: 15))
                            .foregroundStyle(design.inkFaint.color)
                            .frame(width: ComposerBox.slot, height: ComposerBox.slot)
                            .contentShape(Rectangle())
                    }
                    .buttonStyle(.amuxControl)
                    .accessibilityLabel("Clear")
                    .identified("chat.clear", label: "Clear")
                }
                if let strip = model.strip, let chip = ChatWords.chip(strip, model.settings) {
                    modelChip(chip)
                }
                Spacer(minLength: 4)
                primary
            }
            .padding(.leading, -13)
            .padding(.trailing, -(ComposerBox.slot - ComposerBox.round) / 2)
            .padding(.vertical, -(ComposerBox.slot - ComposerBox.round) / 2)
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 11)
        .frosted(RoundedRectangle(cornerRadius: design.metrics.floatRadius, style: .continuous))
        .onDisappear { if model.dictation.active { model.dictation.stop() } }
    }

    private var attachItems: [MenuItem] {
        var items = [
            MenuItem(title: String(localized: "Photo"), systemImage: "photo") { attach(.photo) },
            MenuItem(title: String(localized: "File"), systemImage: "doc") { attach(.file) },
        ]
        if let settings = model.settings, !settings.modes.isEmpty || settings.cycleMode {
            items.append(MenuItem(
                title: ChatWords.permissionsItem(settings), systemImage: "hand.raised",
                action: openSettings))
        }
        return items
    }

    /// The model on one line, the effort and mode under it, so a long model
    /// id never pushes the mode out of sight. The mode that stops asking
    /// reads in red. A tap opens the settings card.
    private func modelChip(_ chip: (model: String, detail: String)) -> some View {
        let warn = model.settings?.modes.first { $0.current }?.stopsAsking == true
        let label = [chip.model, chip.detail].filter { !$0.isEmpty }.joined(separator: " · ")
        return Button(action: openSettings) {
            HStack(spacing: 4) {
                VStack(alignment: .leading, spacing: 0) {
                    if !chip.model.isEmpty {
                        Text(chip.model)
                            .foregroundStyle(design.inkMuted.color)
                    }
                    if !chip.detail.isEmpty {
                        Text(chip.detail)
                            .foregroundStyle(warn ? design.removed.color : design.inkFaint.color)
                    }
                }
                .designFont(.monoSmall, design)
                .lineLimit(1)
                .truncationMode(.middle)
                Image(systemName: "chevron.up.chevron.down")
                    .font(.system(size: 9, weight: .semibold))
                    .foregroundStyle(design.inkFaint.color)
            }
            .frame(minHeight: ComposerBox.slot)
            .contentShape(Rectangle())
        }
        .buttonStyle(.amuxControl)
        .layoutPriority(1)
        .disabled(model.settings == nil)
        .identified("chat.model", label: label)
    }

    private var dictationLabel: String {
        model.dictation.active ? String(localized: "Stop Dictation") : String(localized: "Dictate")
    }

    /// What dictation is doing, and when it was refused, the way to Settings.
    private func dictationLine(_ sentence: String) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(sentence)
                .designFont(.caption, design)
                .foregroundStyle(design.inkMuted.color)
                .fixedSize(horizontal: false, vertical: true)
            if model.dictation.phase == .denied {
                Button { dictate(.dictationSettings) } label: {
                    Text("Open Settings")
                        .designFont(.caption, design)
                        .foregroundStyle(design.accent.color)
                        .thumbTarget(x: 4, y: 12)
                }
                .buttonStyle(.amuxControl)
                .identified("chat.dictationSettings", label: String(localized: "Open Settings"))
                .reclaimingThumbTarget(x: 4, y: 12)
            }
        }
        .identified("chat.dictation", label: sentence, value: "\(model.dictation.phase)")
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
                    .frame(height: ComposerBox.round)
                    .background(Capsule().fill(design.ink.color))
                    .padding(.horizontal, (ComposerBox.slot - ComposerBox.round) / 2)
                    .frame(height: ComposerBox.slot)
                    .contentShape(Rectangle())
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
                .frame(width: ComposerBox.round, height: ComposerBox.round)
                .background(Circle().fill(enabled ? design.ink.color : design.sunken.color))
                .frame(width: ComposerBox.slot, height: ComposerBox.slot)
                .contentShape(Rectangle())
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
        case .text(let name, let text): .text(name: name, lines: UInt32(ChatModel.lines(text)))
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
        let message = Text(text)
            .designFont(.detail, design)
            .foregroundStyle(design.ink.color)
        let status = Text(state)
            .designFont(.caption, design)
            .foregroundStyle(warn ? design.accent.color : design.inkFaint.color)
        return VStack(alignment: .leading, spacing: 6) {
            // The state sits beside the message while both fit on one line. A long reason
            // moves under the message instead of pushing the message out of the row.
            ViewThatFits(in: .horizontal) {
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    message.lineLimit(1)
                    Spacer(minLength: 6)
                    status.lineLimit(1)
                }
                VStack(alignment: .leading, spacing: 2) {
                    message.lineLimit(2)
                    status.lineLimit(2)
                }
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

/// The agent's commands matching a draft that is a leading "/word", one
/// row each; a tap puts the command in the draft.
struct SlashRows: View {
    @Environment(\.design) private var design
    let commands: [CommandView]
    /// Codex's commands are its skills, mentioned by "$name".
    let codex: Bool
    let pick: (CommandView) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            ForEach(commands, id: \.name) { command in
                Button { pick(command) } label: {
                    VStack(alignment: .leading, spacing: 2) {
                        HStack(spacing: 6) {
                            Text(verbatim: (codex ? "$" : "/") + command.name)
                                .designFont(.mono, design)
                                .foregroundStyle(design.ink.color)
                                .lineLimit(1)
                            if !command.argumentHint.isEmpty {
                                Text(verbatim: command.argumentHint)
                                    .designFont(.monoSmall, design)
                                    .foregroundStyle(design.inkFaint.color)
                                    .lineLimit(1)
                            }
                            Spacer(minLength: 0)
                        }
                        if !command.description.isEmpty {
                            Text(verbatim: command.description)
                                .designFont(.detail, design)
                                .foregroundStyle(design.inkMuted.color)
                                .lineLimit(1)
                        }
                    }
                    .padding(.horizontal, 14)
                    .padding(.vertical, 8)
                    .frame(maxWidth: .infinity, minHeight: 44, alignment: .leading)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.amuxControl)
                .accessibilityLabel(ChatWords.command(command))
                .identified("chat.slash.\(command.name)", label: ChatWords.command(command))
            }
        }
        .padding(.vertical, 4)
        .frosted(RoundedRectangle(cornerRadius: design.metrics.floatRadius, style: .continuous), as: .glass)
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
