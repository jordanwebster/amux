import AmuxCore
import AmuxDesign
import SwiftUI

/// The commands raised by typing a slash.
///
/// Not a card you opened: it is raised by what you are writing and it goes
/// away by itself when the writing stops matching, so the conversation behind
/// it is not dimmed and there is nothing here to dismiss. It sits directly on
/// top of the box, close enough that the eye does not travel.
struct SlashRows: View {
    @Environment(\.design) private var design
    let commands: SlashCommands
    let pick: @MainActor (ProviderCommand) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            ForEach(Array(commands.rows.enumerated()), id: \.element.name) { index, command in
                if index > 0 { Divider().overlay(design.hairline.color) }
                row(command)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .frosted(RoundedRectangle(cornerRadius: 21, style: .continuous), wash: 0.88)
        .accessibilityElement(children: .contain)
        .identified("slash", value: commands.typed)
    }

    private func row(_ command: ProviderCommand) -> some View {
        Button { pick(command) } label: {
            HStack(spacing: 8) {
                Text("/\(command.name)")
                    .designFont(.mono, design)
                    .foregroundStyle(design.ink.color)
                    .lineLimit(1)
                    .truncationMode(.middle)
                Spacer(minLength: 6)
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 9)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel("/\(command.name), \(command.origin)")
        .identified("slash.\(command.name)", label: "/\(command.name)", value: command.origin)
    }
}
