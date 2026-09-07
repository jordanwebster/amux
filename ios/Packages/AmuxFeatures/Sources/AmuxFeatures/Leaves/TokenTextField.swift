import AmuxCore
import AmuxDesign
import SwiftUI
import UIKit

/// The field a message is written in, tokens and all.
///
/// The one UIKit view in the app, and the reason is in `docs/IOS.md`: SwiftUI's
/// text editing binds a string, and nothing in it makes a run of that string
/// one object — there is no way to draw a chip between two words, no way to
/// make the caret step over it in one press, and no way to make one backspace
/// take the whole of it. Those three gestures are what the design settled
/// attachments on, so the field is where the exception is spent.
///
/// What crosses the boundary is small on purpose: a `MessageDraft` in and the
/// same draft back. The text view holds no state of its own — what it draws is
/// rebuilt from the draft whenever the draft is not what it is already
/// showing — so the composer stays a function of the conversation's state and
/// a screenshot of it is reproducible.
struct TokenTextField: UIViewRepresentable {
    @Binding var draft: MessageDraft
    let design: Design
    /// True while the door is photographing. A caret blinks on a timer, and a
    /// still of a blink is whichever half the shutter caught.
    let photographed: Bool
    /// How far the field grows before it scrolls inside itself.
    let lines: Int

    func makeUIView(context: Context) -> UITextView {
        let view = UITextView()
        view.delegate = context.coordinator
        view.backgroundColor = .clear
        view.textContainerInset = .zero
        view.textContainer.lineFragmentPadding = 0
        view.contentInset = .zero
        view.isScrollEnabled = true
        view.alwaysBounceVertical = false
        view.showsVerticalScrollIndicator = false
        // The predictive strip above the keyboard rewrites itself as the
        // system thinks about what was typed, and so do smart quotes: both are
        // things on a photographed screen that will not hold still.
        view.autocorrectionType = .no
        view.smartQuotesType = .no
        view.smartDashesType = .no
        view.smartInsertDeleteType = .no
        // A token is one character and it is not a link, an address or a date;
        // letting the system find things inside the text would draw over it.
        view.dataDetectorTypes = []
        context.coordinator.apply(draft, to: view, design: design, photographed: photographed)
        // The chips are drawn once against the appearance they will be read
        // in, so the appearance changing has to draw them again.
        view.registerForTraitChanges([UITraitUserInterfaceStyle.self]) {
            (view: UITextView, _) in
            context.coordinator.redraw(view)
        }
        return view
    }

    func updateUIView(_ view: UITextView, context: Context) {
        context.coordinator.parent = self
        context.coordinator.apply(draft, to: view, design: design, photographed: photographed)
    }

    /// The height the written text asks for, capped.
    ///
    /// Measured here rather than reported back through state: a field that
    /// tells the layout how tall it became, and is then laid out again, has
    /// two resting places and a screenshot of it is a coin toss.
    func sizeThatFits(
        _ proposal: ProposedViewSize, uiView: UITextView, context: Context
    ) -> CGSize? {
        let width = proposal.width ?? uiView.bounds.width
        guard width > 0 else { return nil }
        let asked = uiView.sizeThatFits(
            CGSize(width: width, height: .greatestFiniteMagnitude)).height
        let line = TokenTextField.font(design).lineHeight
        return CGSize(width: width, height: min(max(asked, line), line * CGFloat(lines)))
    }

    func makeCoordinator() -> Coordinator { Coordinator(self) }

    /// The face the field is set in: the design's body role, resolved to a
    /// concrete font because UIKit has no notion of a role.
    static func font(_ design: Design) -> UIFont {
        BundledFonts.register()
        let spec = design.spec(.body)
        let face = UIFont(name: spec.family, size: spec.size)
            ?? .systemFont(ofSize: spec.size)
        return UIFontMetrics(forTextStyle: .callout).scaledFont(for: face)
    }

    @MainActor
    final class Coordinator: NSObject, UITextViewDelegate {
        var parent: TokenTextField
        /// What the view is already showing, so an update that says nothing
        /// new does not reset the caret out from under a finger.
        private var shown: MessageDraft?
        private var design: Design
        private var photographed = false
        /// True while this coordinator is the one changing the view. Setting
        /// the attributed text moves the selection to the start and back, and
        /// the delegate hears both — which would write a caret position nobody
        /// asked for into the draft on the way past.
        private var applying = false

        init(_ parent: TokenTextField) {
            self.parent = parent
            self.design = parent.design
            super.init()
        }

        func apply(
            _ draft: MessageDraft, to view: UITextView, design: Design, photographed: Bool
        ) {
            let restyled = self.design != design || self.photographed != photographed
            self.design = design
            self.photographed = photographed
            view.tintColor = photographed
                ? .clear : design.accent.uiColor(view.appearance)
            guard restyled || shown?.body != draft.body || shown?.tokens != draft.tokens else {
                shown = draft
                return
            }
            shown = draft
            let selection = view.selectedRange
            applying = true
            view.attributedText = attributed(draft, in: view)
            view.typingAttributes = Self.plain(design, view)
            let caret = min(draft.caret, view.text.utf16.count)
            view.selectedRange = view.isFirstResponder
                ? selection : NSRange(location: caret, length: 0)
            applying = false
        }

        /// Draws the chips again for an appearance nobody asked the field
        /// about: they are pictures, so they do not resolve a dynamic colour
        /// the way drawn text does.
        func redraw(_ view: UITextView) {
            guard let draft = shown else { return }
            let selection = view.selectedRange
            applying = true
            view.attributedText = attributed(draft, in: view)
            view.typingAttributes = Self.plain(design, view)
            view.tintColor = photographed
                ? .clear : design.accent.uiColor(view.appearance)
            view.selectedRange = selection
            applying = false
        }

        func textViewDidChange(_ view: UITextView) {
            guard !applying else { return }
            var draft = parent.draft
            let (body, caret) = read(view)
            draft.body = body
            draft.place(caret: caret)
            shown = draft
            parent.draft = draft
        }

        func textViewDidChangeSelection(_ view: UITextView) {
            guard !applying else { return }
            // Typing after a token must not inherit the token: an attachment
            // left in the typing attributes puts a second chip in the sentence
            // for every letter typed after the first.
            view.typingAttributes = Self.plain(design, view)
            var draft = parent.draft
            let (body, caret) = read(view)
            guard body == draft.body else { return }
            draft.place(caret: caret)
            shown = draft
            parent.draft = draft
        }

        /// What the view is showing, back in the draft's own spelling: every
        /// attachment is the character its token stands in as, and everything
        /// else is itself. The caret comes back with it, counted in characters
        /// rather than in the UTF-16 units the platform reports, because the
        /// draft is indexed the way a person reads it.
        private func read(_ view: UITextView) -> (String, Int) {
            let text = view.attributedText ?? NSAttributedString()
            let at = view.selectedRange.location
            var body = ""
            var caret: Int?
            text.enumerateAttributes(in: NSRange(location: 0, length: text.length)) {
                attributes, range, _ in
                let piece = text.attributedSubstring(from: range).string
                let written = (attributes[.amuxToken] as? String)
                    .map { String(repeating: $0, count: piece.count) } ?? piece
                if caret == nil {
                    if at <= range.location {
                        caret = body.count
                    } else if at < range.location + range.length {
                        caret = body.count + Self.characters(
                            upTo: at - range.location, in: piece, standingIn: written)
                    }
                }
                body += written
            }
            return (body, caret ?? body.count)
        }

        /// How many characters of a run lie before a platform offset into it.
        ///
        /// A token stands in for a run of one character however many units the
        /// platform counts it as, so it is either in front of the caret or
        /// behind it and never split.
        private static func characters(
            upTo units: Int, in piece: String, standingIn written: String
        ) -> Int {
            guard written == piece else { return units > 0 ? written.count : 0 }
            guard let end = Range(NSRange(location: 0, length: units), in: piece)?.upperBound
            else { return 0 }
            return piece.distance(from: piece.startIndex, to: end)
        }

        private func attributed(_ draft: MessageDraft, in view: UITextView) -> NSAttributedString {
            let plain = Self.plain(design, view)
            let built = NSMutableAttributedString()
            for character in draft.body {
                guard let token = draft.tokens[character] else {
                    built.append(NSAttributedString(string: String(character), attributes: plain))
                    continue
                }
                let attachment = NSTextAttachment()
                guard let chip = TokenChipImage.draw(
                    token, design: design, appearance: view.appearance,
                    scale: view.traitCollection.displayScale,
                    font: TokenTextField.font(design))
                else { continue }
                attachment.image = chip.image
                attachment.bounds = chip.bounds
                let run = NSMutableAttributedString(attachment: attachment)
                run.addAttribute(
                    .amuxToken, value: String(character),
                    range: NSRange(location: 0, length: run.length))
                built.append(run)
            }
            return built
        }

        private static func plain(_ design: Design, _ view: UITextView)
            -> [NSAttributedString.Key: Any]
        {
            [.font: TokenTextField.font(design),
             .foregroundColor: design.ink.uiColor(view.appearance)]
        }
    }
}

extension NSAttributedString.Key {
    /// Which token an attachment run stands for. It travels with the run, so
    /// dragging a chip elsewhere in the sentence moves the token with it.
    static let amuxToken = NSAttributedString.Key("amuxToken")
}

extension UIView {
    /// The appearance this view is being drawn in, in the design's own word.
    var appearance: Appearance {
        traitCollection.userInterfaceStyle == .dark ? .dark : .light
    }
}

/// One token, drawn as a picture so it can sit inside a line of text.
///
/// A chip in the middle of a sentence is one character to the caret, and the
/// only thing a run of text can carry that is one character and several words
/// wide is an attachment. The chip itself is the same `TokenChip` the feed
/// draws — rendered rather than redrawn, so an attachment somebody wrote and
/// an attachment an agent sent are one thing described once.
@MainActor
enum TokenChipImage {
    static func draw(
        _ token: DraftToken, design: Design, appearance: Appearance, scale: CGFloat, font: UIFont
    ) -> (image: UIImage, bounds: CGRect)? {
        let renderer = ImageRenderer(
            content: TokenChip(token: token, appearance: appearance).environment(\.design, design))
        renderer.scale = scale
        guard let image = renderer.uiImage else { return nil }
        // Sat on the text's own baseline rather than on the line's bottom, so
        // a chip between two words reads as one of them.
        return (image, CGRect(
            x: 0, y: font.descender - (image.size.height - font.lineHeight) / 2,
            width: image.size.width, height: image.size.height))
    }
}
