import AmuxCore
import AmuxDesign
import SwiftUI

/// What the plus offers.
///
/// Three things and no more. Paste is not here — the keyboard already pastes,
/// and the composer turns a long one into a token by itself — and neither is a
/// slash command, which is typed. A menu row for something the keyboard
/// already does is dead weight.
public enum AttachChoice: Equatable, Sendable {
    case photo
    case file
    /// What this agent may do without asking. It is behind the plus rather
    /// than in the footer because it decides what the message you are about to
    /// send is allowed to do, which makes it part of writing the message.
    case permissions
}

/// The plus, opened.
///
/// One card holding two tiles and a row, rather than a menu or a scatter of
/// bubbles. The tiles are the two things that put something *in* the message;
/// the row is the one thing that is about the message without being in it, so
/// it is drawn as a row and not as a third tile.
struct PlusCard: View {
    @Environment(\.design) private var design
    /// What the agent may do now, in its provider's words. Absent where the
    /// layer does not report one, and then the row says nothing rather than
    /// guessing at a default.
    let permission: ProviderPermission
    let choose: @MainActor (AttachChoice) -> Void

    var body: some View {
        VStack(spacing: 8) {
            HStack(spacing: 8) {
                tile(.photo, glyph: "photo", label: "Photo")
                tile(.file, glyph: "doc", label: "File")
            }
            permissions
        }
        .padding(8)
        .frosted(RoundedRectangle(cornerRadius: design.metrics.floatRadius, style: .continuous))
        .accessibilityElement(children: .contain)
        .identified("plus")
    }

    private func tile(_ choice: AttachChoice, glyph: String, label: String) -> some View {
        Button { choose(choice) } label: {
            VStack(spacing: 10) {
                Image(systemName: glyph)
                    .font(.system(size: 26, weight: .regular))
                    .foregroundStyle(design.ink.color)
                Text(label)
                    .designFont(.body, design)
                    .foregroundStyle(design.inkMuted.color)
            }
            .frame(maxWidth: .infinity)
            .padding(.vertical, 22)
            .background {
                RoundedRectangle(cornerRadius: design.metrics.controlRadius, style: .continuous)
                    .fill(design.sunken.color)
            }
            .contentShape(RoundedRectangle(
                cornerRadius: design.metrics.controlRadius, style: .continuous))
        }
        .buttonStyle(.plain)
        .accessibilityLabel(label)
        .identified("plus.\(label.lowercased())", label: label)
    }

    private var permissions: some View {
        Button { choose(.permissions) } label: {
            HStack(spacing: 12) {
                Image(systemName: "lock")
                    .font(.system(size: 16, weight: .regular))
                    .foregroundStyle(design.ink.color)
                    .frame(width: 22)
                Text("Permissions")
                    .designFont(.body, design)
                    .foregroundStyle(design.ink.color)
                Spacer(minLength: 8)
                if let current = permission.current {
                    Text(current)
                        .designFont(.mono, design)
                        .foregroundStyle(design.inkFaint.color)
                        .lineLimit(1)
                }
                Image(systemName: "chevron.right")
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundStyle(design.inkFaint.color)
            }
            .padding(.horizontal, 14)
            .frame(minHeight: 52)
            .background {
                RoundedRectangle(cornerRadius: design.metrics.controlRadius, style: .continuous)
                    .fill(design.sunken.color)
            }
            .contentShape(RoundedRectangle(
                cornerRadius: design.metrics.controlRadius, style: .continuous))
        }
        .buttonStyle(.plain)
        .accessibilityLabel(
            ["Permissions", permission.current].compactMap { $0 }.joined(separator: ", "))
        .identified("plus.permissions", label: "Permissions", value: permission.current ?? "")
    }
}
