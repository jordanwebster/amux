import AmuxCore
import AmuxDesign
import SwiftUI

/// Where a conversation runs, in full.
///
/// The pill has one short line for the machine and the directory, and that
/// line is shortened on purpose. This is where the whole of each is, along
/// with the address another agent or a terminal writes to, and each can be
/// selected and copied as it stands.
struct PlaceSheet: View {
    @Environment(\.design) private var design
    let subject: ConversationSubject

    /// Tall enough for the three facts at an ordinary text size. A larger size
    /// scrolls inside it rather than growing the sheet over the conversation.
    static let height: CGFloat = 280

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                Text(subject.name)
                    .designFont(.screenTitle, design)
                    .foregroundStyle(design.ink.color)
                    .fixedSize(horizontal: false, vertical: true)
                    .identified("place.title", value: subject.name)
                VStack(alignment: .leading, spacing: 0) {
                    if let host = subject.host {
                        fact("Host", host, id: "host")
                        rule
                    }
                    if !subject.directory.isEmpty {
                        fact("Directory", subject.directory, id: "directory")
                        rule
                    }
                    fact("Address", subject.address, id: "address")
                }
                .background {
                    RoundedRectangle(cornerRadius: design.metrics.cardRadius, style: .continuous)
                        .fill(design.raised.color)
                }
            }
            .padding(.horizontal, design.metrics.gutter)
            .padding(.top, 24)
            .padding(.bottom, 16)
        }
        .scrollBounceBehavior(.basedOnSize)
        .background(design.ground.color)
        .accessibilityElement(children: .contain)
        .identified("place", value: subject.address)
    }

    private func fact(_ label: String, _ value: String, id: String) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            Text(label)
                .designFont(.caption, design)
                .foregroundStyle(design.inkFaint.color)
            Text(value)
                .designFont(.mono, design)
                .foregroundStyle(design.ink.color)
                .fixedSize(horizontal: false, vertical: true)
                .textSelection(.enabled)
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 11)
        .frame(maxWidth: .infinity, minHeight: 44, alignment: .leading)
        .accessibilityElement(children: .combine)
        .identified("place.\(id)", label: label, value: value)
    }

    private var rule: some View {
        Rectangle()
            .fill(design.hairline.color)
            .frame(height: design.metrics.hairline)
            .padding(.leading, 14)
    }
}
