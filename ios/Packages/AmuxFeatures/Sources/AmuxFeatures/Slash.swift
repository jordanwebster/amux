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
        .frosted(RoundedRectangle(cornerRadius: design.metrics.floatRadius, style: .continuous))
        .accessibilityElement(children: .contain)
        .identified("slash", value: commands.typed)
    }

    private func row(_ command: ProviderCommand) -> some View {
        Button { pick(command) } label: {
            HStack(spacing: 12) {
                Text("/\(command.name)")
                    .designFont(.mono, design)
                    .foregroundStyle(design.ink.color)
                    .lineLimit(1)
                    .truncationMode(.middle)
                Spacer(minLength: 12)
                // Two sessions can both offer /compact and mean different
                // things by it, and one of them can come from a plugin
                // somebody installed. Which is which is not something the app
                // can work out for you after you have picked the wrong one.
                if !command.origin.isEmpty {
                    Text(command.origin)
                        .designFont(.detail, design)
                        .foregroundStyle(design.inkFaint.color)
                        .lineLimit(1)
                }
            }
            .padding(.horizontal, 16)
            .frame(minHeight: 52)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel("/\(command.name), \(command.origin)")
        .identified("slash.\(command.name)", label: "/\(command.name)", value: command.origin)
    }
}
