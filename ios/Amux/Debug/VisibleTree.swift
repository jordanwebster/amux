import Foundation
import UIKit

/// What is on screen, read from the accessibility tree.
///
/// The tree, and not the view hierarchy: a SwiftUI screen draws most of itself
/// into a handful of views, and its identifiers, labels and values live on
/// accessibility elements hanging off them. Reading the same tree a journey
/// drives and VoiceOver speaks means a door query cannot claim something is
/// reachable that a person could not reach.
enum VisibleTree {
    @MainActor
    static func elements(of window: UIWindow) -> [VisibleElement] {
        var found: [VisibleElement] = []
        var seen = Set<ObjectIdentifier>()
        walk(window, in: window, into: &found, seen: &seen)
        return found
    }

    /// The first element carrying an identifier, depth-first — the same order
    /// a query reports, so what a driver taps is what it just read.
    @MainActor
    static func find(_ identifier: String, in window: UIWindow) -> NSObject? {
        var found: NSObject?
        var seen = Set<ObjectIdentifier>()
        search(window, matching: identifier, into: &found, seen: &seen)
        return found
    }

    @MainActor
    private static func walk(
        _ node: NSObject, in window: UIWindow,
        into found: inout [VisibleElement], seen: inout Set<ObjectIdentifier>
    ) {
        guard seen.insert(ObjectIdentifier(node)).inserted else { return }
        if let element = describe(node, in: window) { found.append(element) }
        for child in children(of: node) {
            walk(child, in: window, into: &found, seen: &seen)
        }
    }

    @MainActor
    private static func search(
        _ node: NSObject, matching identifier: String,
        into found: inout NSObject?, seen: inout Set<ObjectIdentifier>
    ) {
        guard found == nil, seen.insert(ObjectIdentifier(node)).inserted else { return }
        if (node as? any UIAccessibilityIdentification)?.accessibilityIdentifier == identifier {
            found = node
            return
        }
        for child in children(of: node) {
            search(child, matching: identifier, into: &found, seen: &seen)
        }
    }

    /// An element's accessibility children first, then the subviews it draws
    /// into. Both, because a hosting view has accessibility children and real
    /// subviews, and a control the app builds in UIKit has only subviews.
    @MainActor
    private static func children(of node: NSObject) -> [NSObject] {
        var children: [NSObject] = []
        if let listed = node.accessibilityElements as? [NSObject] {
            children.append(contentsOf: listed)
        } else {
            let count = node.accessibilityElementCount()
            if count != NSNotFound && count > 0 {
                for index in 0..<count {
                    if let child = node.accessibilityElement(at: index) as? NSObject {
                        children.append(child)
                    }
                }
            }
        }
        if let view = node as? UIView {
            children.append(contentsOf: view.subviews)
        }
        return children
    }

    @MainActor
    private static func describe(_ node: NSObject, in window: UIWindow) -> VisibleElement? {
        let identifier = (node as? any UIAccessibilityIdentification)?.accessibilityIdentifier
        let label = node.accessibilityLabel
        // A view that names nothing and says nothing is scaffolding; reporting
        // it would bury the elements a journey actually asserts on.
        guard let identifier, !identifier.isEmpty else {
            guard let label, !label.isEmpty, node.isAccessibilityElement else { return nil }
            return element(node, identifier: "", label: label, in: window)
        }
        return element(node, identifier: identifier, label: label, in: window)
    }

    @MainActor
    private static func element(
        _ node: NSObject, identifier: String, label: String?, in window: UIWindow
    ) -> VisibleElement {
        let frame = if let view = node as? UIView {
            view.convert(view.bounds, to: window)
        } else {
            window.convert(node.accessibilityFrame, from: nil)
        }
        let enabled = if let control = node as? UIControl {
            control.isEnabled
        } else {
            !node.accessibilityTraits.contains(.notEnabled)
        }
        return VisibleElement(
            identifier: identifier,
            label: label.flatMap { $0.isEmpty ? nil : $0 },
            value: node.accessibilityValue.flatMap { $0.isEmpty ? nil : $0 },
            frame: VisibleFrame(
                x: frame.origin.x, y: frame.origin.y,
                width: frame.width, height: frame.height),
            enabled: enabled)
    }
}
