import UIKit
import XCTest
@testable import Amux

/// What the debug door acts on when a driver taps or scrolls: only what a
/// person looking at the screen could reach.
@MainActor
final class DoorTargetTests: XCTestCase {
    private var window: UIWindow!

    override func setUp() async throws {
        let scene = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }.first
        window = scene.map { UIWindow(windowScene: $0) } ?? UIWindow()
        window.frame = CGRect(x: 0, y: 0, width: 390, height: 844)
        window.rootViewController = UIViewController()
        window.isHidden = false
    }

    override func tearDown() async throws {
        window.isHidden = true
        window = nil
    }

    private var root: UIView { window.rootViewController!.view }

    /// An element the way SwiftUI draws one: not a view of its own, but an
    /// accessibility element hanging off the view that hosts the screen.
    @discardableResult
    private func element(_ label: String, _ frame: CGRect, in screen: UIView) -> NSObject {
        let element = UIAccessibilityElement(accessibilityContainer: screen)
        element.accessibilityLabel = label
        element.accessibilityFrame = window.convert(frame, to: nil)
        screen.accessibilityElements = (screen.accessibilityElements ?? []) + [element]
        return element
    }

    private func screen(hidden: Bool = false) -> UIView {
        let screen = UIView(frame: root.bounds)
        screen.backgroundColor = .systemBackground
        screen.isHidden = hidden
        root.addSubview(screen)
        return screen
    }

    @discardableResult
    private func list(_ frame: CGRect, content: CGSize, in parent: UIView) -> UIScrollView {
        let list = UIScrollView(frame: frame)
        list.contentSize = content
        list.contentInsetAdjustmentBehavior = .never
        parent.addSubview(list)
        return list
    }

    private let point = CGPoint(x: 195, y: 300)

    /// A hidden tab keeps its elements where they were drawn: its small
    /// Sign In button is not what a tap on the fleet row reaches.
    func testATapFindsNothingOnAHiddenScreen() {
        let you = screen(hidden: true)
        element("Sign In", CGRect(x: 150, y: 280, width: 90, height: 44), in: you)
        let fleet = screen()
        let row = element("worker-2, needs you", CGRect(x: 0, y: 260, width: 390, height: 80), in: fleet)

        XCTAssertIdentical(VisibleTree.element(at: point, in: window, saying: "worker-2"), row)
        XCTAssertIdentical(VisibleTree.element(at: point, in: window), row)
        XCTAssertIdentical(VisibleTree.element(at: point, in: window, saying: "Sign In"), row)
    }

    /// The page under a pushed one is drawn but covered: an ask option's
    /// tap does not reach the host row beneath it.
    func testATapFindsNothingOnACoveredScreen() {
        let hosts = screen()
        element("Studio", CGRect(x: 0, y: 290, width: 390, height: 20), in: hosts)
        let chat = screen()
        let option = element("Allow once", CGRect(x: 20, y: 270, width: 350, height: 60), in: chat)

        XCTAssertIdentical(VisibleTree.element(at: point, in: window, saying: "Allow once"), option)
        XCTAssertIdentical(VisibleTree.element(at: point, in: window), option)
        XCTAssertIdentical(VisibleTree.element(at: point, in: window, saying: "Studio"), option)
    }

    /// A long card docked over the chat scrolls, not the chat behind it.
    func testAScrollMovesTheCardOnTop() {
        let chat = screen()
        let feed = list(chat.bounds, content: CGSize(width: 390, height: 4000), in: chat)
        list(CGRect(x: 0, y: 60, width: 390, height: 44), content: CGSize(width: 900, height: 44), in: chat)
        let card = list(
            CGRect(x: 0, y: 444, width: 390, height: 400), content: CGSize(width: 390, height: 900),
            in: chat)

        XCTAssertIdentical(VisibleTree.list(in: window), card)
        card.removeFromSuperview()
        XCTAssertIdentical(VisibleTree.list(in: window), feed)
    }

    /// A card with nothing more to show leaves the chat to scroll, and a
    /// hidden screen's list is never the one moved.
    func testAScrollSkipsListsThatCannotMoveOrCannotBeSeen() {
        let other = screen(hidden: true)
        list(other.bounds, content: CGSize(width: 390, height: 4000), in: other)
        let chat = screen()
        let feed = list(chat.bounds, content: CGSize(width: 390, height: 4000), in: chat)
        list(CGRect(x: 0, y: 644, width: 390, height: 200), content: CGSize(width: 390, height: 200), in: chat)

        XCTAssertIdentical(VisibleTree.list(in: window), feed)
    }
}
