import SwiftUI
import UIKit

/// Keeps the row at the top of a reader's view where it is on screen while
/// the rows around it change: rows taken in or let go at either end of the
/// drawn run, a page landing, the notice above the oldest row coming or
/// going. Whenever layout
/// moves the pinned row within the rows, the list scrolls by as much, so a
/// change above the view moves nothing in it and a change below it cannot.
/// A reader following the newest row is kept at the bottom instead.
@MainActor
final class ScrollAnchor {
    static let rows = "chat.rows"
    /// Where each drawn row the list has laid out is, in the rows.
    private var frames: [String: CGRect] = [:]
    /// The row at the top of the view and where it was last laid out.
    private var pinned: (id: String, top: CGFloat)?
    /// The UIKit scroll view under the list. SwiftUI's scroll position
    /// moves the list a frame or more after it is asked to, which would show
    /// the rows moved in between; this moves it within the layout pass that
    /// moved them.
    weak var scrollView: UIScrollView?
    /// Where the list last moved itself to: that is not the reader scrolling.
    private var moving: CGFloat?

    /// A row was laid out. When it is the pinned row and it moved, the list
    /// scrolls by as much, unless it is following the newest row.
    func laidOut(_ id: String, at frame: CGRect, following: Bool) {
        frames[id] = frame
        guard let pinned, pinned.id == id else { return }
        self.pinned = (id, frame.minY)
        let moved = frame.minY - pinned.top
        guard moved != 0, !following, let scrollView else { return }
        scrollView.contentOffset.y += moved
        moving = scrollView.contentOffset.y
    }

    /// Whether the list is where it last moved itself to.
    func movedItself(to offset: CGFloat) -> Bool {
        guard let moving else { return false }
        if abs(offset - moving) < 0.5 { return true }
        self.moving = nil
        return false
    }

    var pinning: Bool { pinned != nil }

    func gone(_ id: String) {
        frames[id] = nil
        if pinned?.id == id { pinned = nil }
    }

    /// Pins the row that crosses the top of the view, or failing that the
    /// first one below it, and names it when that is a different row.
    func pick(top: CGFloat) -> String? {
        let below = frames.filter { $0.value.maxY > top }
        guard let row = below.min(by: { $0.value.minY < $1.value.minY }) else {
            pinned = nil
            return nil
        }
        let changed = pinned?.id != row.key
        pinned = (row.key, row.value.minY)
        return changed ? row.key : nil
    }
}

/// Finds the UIKit scroll view a list's rows are laid out in.
struct ScrollViewFinder: UIViewRepresentable {
    let found: (UIScrollView) -> Void

    func makeUIView(context: Context) -> Finder { Finder(found: found) }
    func updateUIView(_ view: Finder, context: Context) {}

    final class Finder: UIView {
        let found: (UIScrollView) -> Void

        init(found: @escaping (UIScrollView) -> Void) {
            self.found = found
            super.init(frame: .zero)
            isUserInteractionEnabled = false
        }

        required init?(coder: NSCoder) { nil }

        override func didMoveToWindow() {
            super.didMoveToWindow()
            var view = superview
            while let next = view, !(next is UIScrollView) { view = next.superview }
            if let scrollView = view as? UIScrollView { found(scrollView) }
        }
    }
}
