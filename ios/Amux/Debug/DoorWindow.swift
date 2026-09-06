import Foundation
import QuartzCore
import UIKit

/// The window a query or a capture is about.
enum DoorWindow {
    @MainActor
    static var current: UIWindow? {
        let scenes = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }
        let windows = scenes.flatMap(\.windows)
        return windows.first(where: \.isKeyWindow) ?? windows.first
    }

    /// The first text input inside a view, which is what a SwiftUI text field
    /// actually is once it has been laid out.
    @MainActor
    static func textInput(in view: UIView) -> (any UIKeyInput & UIResponder)? {
        if let input = view as? (any UIKeyInput & UIResponder) { return input }
        for subview in view.subviews {
            if let found = textInput(in: subview) { return found }
        }
        return nil
    }
}

/// Waiting for the display rather than for a duration: a fixed sleep is either
/// a flake on a slow machine or wasted seconds on a fast one.
enum DoorFrames {
    @MainActor
    static func next() async {
        await withCheckedContinuation { continuation in
            Waiter.wait(continuation)
        }
    }

    @MainActor
    private final class Waiter {
        private var link: CADisplayLink?
        private var continuation: CheckedContinuation<Void, Never>?
        private static var live: [Waiter] = []

        static func wait(_ continuation: CheckedContinuation<Void, Never>) {
            let waiter = Waiter()
            live.append(waiter)
            waiter.continuation = continuation
            let link = CADisplayLink(target: waiter, selector: #selector(fired))
            link.add(to: .main, forMode: .common)
            waiter.link = link
        }

        @objc private func fired() {
            link?.invalidate()
            link = nil
            let waiting = continuation
            continuation = nil
            Self.live.removeAll { $0 === self }
            waiting?.resume()
        }
    }
}

/// A PNG of the composited window: the material, the blur and the text as the
/// display shows them, not a redraw of the view tree that would lose them.
enum DoorCapture {
    /// Draws the window into an image, exactly as a capture does.
    ///
    /// Rendering the hierarchy is itself what makes a system material resolve
    /// its backdrop for this renderer, and the first pass after an appearance
    /// change resolves it against the appearance before — a light screen comes
    /// back wearing the dark screen's plates. So a capture is this, repeated
    /// until two passes agree.
    @MainActor
    static func render(of window: UIWindow) -> UIImage? {
        let format = UIGraphicsImageRendererFormat()
        format.scale = window.traitCollection.displayScale
        format.opaque = true
        let renderer = UIGraphicsImageRenderer(bounds: window.bounds, format: format)
        var drew = false
        let image = renderer.image { _ in
            drew = window.drawHierarchy(in: window.bounds, afterScreenUpdates: true)
        }
        return drew ? image : nil
    }

    @MainActor
    static func write(_ image: UIImage, _ data: Data, to path: String) -> DoorReply {
        let url = URL(fileURLWithPath: path)
        do {
            try FileManager.default.createDirectory(
                at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
            try data.write(to: url)
        } catch {
            return .error("capture failed: \(error.localizedDescription)")
        }
        let scale = image.scale
        return .captured(
            path: path,
            width: Int((image.size.width * scale).rounded()),
            height: Int((image.size.height * scale).rounded()),
            scale: Int(scale.rounded()))
    }
}
