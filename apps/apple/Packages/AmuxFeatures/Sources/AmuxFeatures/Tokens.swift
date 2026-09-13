import AmuxCore
import AmuxDesign
import SwiftUI

/// One attachment, wherever it appears.
///
/// The same view is drawn twice over: inside the field, where a message is
/// being written, and in the feed, where a message already sent is being read.
/// That is deliberate and it is the whole reason this is a view of its own —
/// an agent can attach things too, and a photo it sent has to look like a
/// photo you sent, because they are the same kind of thing in the same model.
///
/// It says the kind with a mark and the thing with words, and nothing else: a
/// token is a name in a sentence, not a preview. Eighteen kilobytes of pasted
/// log drawn as a wide card was the app disagreeing with its own model, which
/// says a long paste is named rather than shown.
struct TokenChip: View {
    @Environment(\.design) private var design
    let token: DraftToken
    /// Which appearance to resolve the colours in, when the drawing is not
    /// happening on screen. Nil is the ordinary case: the chip is in a view
    /// hierarchy and the tokens resolve themselves.
    var appearance: Appearance?

    var body: some View {
        HStack(spacing: 5) {
            Image(systemName: token.glyph)
                .font(.system(size: 11, weight: .medium))
                .foregroundStyle(colour(design.inkFaint))
            Text(token.label)
                .designFont(.caption, design)
                .foregroundStyle(colour(design.inkMuted))
                .lineLimit(1)
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 3)
        .background {
            Capsule().fill(colour(design.sunken))
        }
        .overlay {
            Capsule().strokeBorder(colour(design.hairline), lineWidth: 1)
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel(token.label)
    }

    private func colour(_ ramp: Ramp) -> Color {
        appearance.map { ramp.color($0) } ?? ramp.color
    }
}

extension DraftToken {
    /// The mark that says what kind of thing this is. The words on the chip
    /// say which one it is; the mark says what sort.
    var glyph: String {
        switch kind {
        case .photo: "photo"
        case .file: "doc"
        case .text: "text.alignleft"
        case .review: "checkmark.bubble"
        case .command: "chevron.forward"
        }
    }

    /// What a message already sent carries, read back through the shared
    /// parser rather than remembered.
    ///
    /// A message in the feed is text, the same text every other client reads,
    /// so what it is drawn as here comes from asking the shared library what
    /// it says — never from a second record kept beside it.
    init?(_ mention: Mention) {
        switch mention.kind {
        case .image:
            self.init(kind: .photo, label: mention.name, element: "", attachment: nil)
        case .file:
            self.init(kind: .file, label: mention.name, element: "", attachment: nil)
        case .text(_, let lines):
            self.init(
                kind: .text, label: "\(mention.name.capitalizedFirst) \u{00B7} \(lines) lines",
                element: "", attachment: nil)
        case .review(_, let comments):
            self.init(
                kind: .review,
                label: comments.count == 1
                    ? "Review \u{00B7} 1 comment" : "Review \u{00B7} \(comments.count) comments",
                element: "", attachment: nil)
        }
    }
}

/// A message read as a reader reads it: what was said, and what was attached
/// to it, in the order they appear.
///
/// Nothing here decides what an attachment is. The text is handed to the same
/// parser every client reads messages with, and a candidate that parser does
/// not accept stays prose byte for byte — which is exactly what a reader on
/// another client will see.
struct AttachedText<Body: View>: View {
    @Environment(\.design) private var design
    let text: String
    /// How the prose between the tokens is drawn. A prompt sets it as one
    /// thing and a reply as another, and neither is this view's business.
    @ViewBuilder let prose: (String) -> Body

    var body: some View {
        let pieces = Bridge.attachments(in: text)
        if pieces.count == 1, case .prose(let only) = pieces[0] {
            prose(only)
        } else {
            VStack(alignment: .leading, spacing: 8) {
                ForEach(Array(pieces.enumerated()), id: \.offset) { _, piece in
                    switch piece {
                    case .prose(let said):
                        let trimmed = said.trimmingCharacters(in: .whitespacesAndNewlines)
                        if !trimmed.isEmpty { prose(trimmed) }
                    case .mention(let mention):
                        if let token = DraftToken(mention) {
                            TokenChip(token: token)
                        }
                    }
                }
            }
        }
    }
}
