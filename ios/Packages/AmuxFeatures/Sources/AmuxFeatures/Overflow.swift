import AmuxCore
import AmuxDesign
import SwiftUI

/// Everything a conversation can be done *to* rather than said to.
public enum OverflowChoice: Equatable, Sendable {
    case rename
    /// The agent's address on the fleet, on the clipboard, so it can be
    /// written to from somewhere else — another agent, a script, a terminal.
    case copyAddress
    case delete
}

/// The overflow, opened under the control that opened it.
///
/// Three rows and no more. Everything here is about the agent as a thing that
/// exists rather than about the conversation you are having with it, which is
/// why none of them is in the composer. Deleting is last and is the only
/// coloured row, because it is the only one that cannot be undone.
struct OverflowMenu: View {
    @Environment(\.design) private var design
    /// What the agent answers to elsewhere: "refactor-auth/studio".
    let address: String
    let choose: @MainActor (OverflowChoice) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            row(.rename, glyph: "pencil", label: "Rename")
            Divider().overlay(design.hairline.color).padding(.leading, 56)
            row(.copyAddress, glyph: "at", label: "Copy Address", detail: address)
            Divider().overlay(design.hairline.color).padding(.leading, 56)
            row(.delete, glyph: "trash", label: "Delete Agent", destructive: true)
        }
        .frame(maxWidth: 300, alignment: .leading)
        .frosted(RoundedRectangle(cornerRadius: design.metrics.cardRadius, style: .continuous))
        .accessibilityElement(children: .contain)
        .identified("overflow", value: address)
    }

    private func row(
        _ choice: OverflowChoice, glyph: String, label: String,
        detail: String? = nil, destructive: Bool = false
    ) -> some View {
        Button { choose(choice) } label: {
            HStack(spacing: 14) {
                Image(systemName: glyph)
                    .font(.system(size: 17, weight: .regular))
                    .foregroundStyle(destructive ? design.removed.color : design.inkMuted.color)
                    .frame(width: 24)
                VStack(alignment: .leading, spacing: 1) {
                    Text(label)
                        .designFont(.body, design)
                        .foregroundStyle(destructive ? design.removed.color : design.ink.color)
                    if let detail {
                        Text(detail)
                            .designFont(.monoSmall, design)
                            .foregroundStyle(design.inkFaint.color)
                            .lineLimit(1)
                    }
                }
                Spacer(minLength: 0)
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 12)
            .frame(minHeight: 52)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel([label, detail].compactMap { $0 }.joined(separator: ", "))
        .identified(
            "overflow.\(label.lowercased().replacingOccurrences(of: " ", with: "-"))",
            label: label, value: detail ?? "")
    }
}

/// Deleting an agent, with what that does spelled out.
///
/// Three consequences, ticked or crossed, because "are you sure" asks a
/// question the reader has no way to answer: what is at stake is which of the
/// agent's effects survive, and only two of the three are reversible by any
/// means at all. The edits it made stay on disk — that is the reassuring one
/// and it goes first, because it is the fear people actually arrive with.
struct DeleteAgentCard: View {
    @Environment(\.design) private var design
    let name: String
    let cancel: @MainActor () -> Void
    let confirm: @MainActor () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("Delete \(name)?")
                .designFont(.screenTitle, design)
                .foregroundStyle(design.ink.color)
                .fixedSize(horizontal: false, vertical: true)
            VStack(alignment: .leading, spacing: 10) {
                consequence("checkmark", kept: true, "Its edits stay. Nothing is reverted.")
                consequence("xmark", kept: false, "Its session ends. Unfinished work stops.")
                consequence(
                    "xmark", kept: false, "The conversation is deleted on every device.")
            }
            HStack(spacing: 10) {
                Button(action: cancel) {
                    Text("Cancel")
                        .designFont(.bodyEmphasis, design)
                        .foregroundStyle(design.ink.color)
                        .frame(maxWidth: .infinity)
                        .frame(height: 52)
                        .background {
                            RoundedRectangle(
                                cornerRadius: design.metrics.controlRadius, style: .continuous)
                                .fill(design.sunken.color)
                        }
                }
                .buttonStyle(.plain)
                .identified("agent-delete.cancel", label: "Cancel")
                Button(action: confirm) {
                    Text("Delete")
                        .designFont(.bodyEmphasis, design)
                        .foregroundStyle(design.onAccent.color)
                        .frame(maxWidth: .infinity)
                        .frame(height: 52)
                        .background {
                            RoundedRectangle(
                                cornerRadius: design.metrics.controlRadius, style: .continuous)
                                .fill(design.removed.color)
                        }
                }
                .buttonStyle(.plain)
                .identified("agent-delete.confirm", label: "Delete")
            }
        }
        .padding(20)
        .frame(maxWidth: .infinity, alignment: .leading)
        .frosted(RoundedRectangle(cornerRadius: design.metrics.floatRadius, style: .continuous))
        .accessibilityElement(children: .contain)
        .identified("agent-delete", value: name)
    }

    private func consequence(_ glyph: String, kept: Bool, _ text: String) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 10) {
            Image(systemName: glyph)
                .font(.system(size: 13, weight: .semibold))
                .foregroundStyle(kept ? design.inkMuted.color : design.removed.color)
                .frame(width: 16)
            Text(text)
                .designFont(.detail, design)
                .foregroundStyle(design.inkMuted.color)
                .fixedSize(horizontal: false, vertical: true)
        }
        .accessibilityElement(children: .combine)
    }
}
