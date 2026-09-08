import Foundation
import XCTest

/// Every control this app draws, checked for the two things a control has to
/// have before anybody can use it without looking at it: a name VoiceOver can
/// read out, and enough of it to hit.
///
/// It is a UI test rather than something the app asks itself, and it has to
/// be. SwiftUI builds an accessibility tree only for an attached accessibility
/// client, and an app is not one from inside its own process: asked from in
/// there, every control answers with no traits at all and nothing can be told
/// from a label. XCUITest is that client, so the element kind, the name and
/// the rectangle read here are the ones VoiceOver and a thumb would get.
///
/// The states it sweeps are whatever the build draws today — asked of the door
/// rather than listed here, so a screen that lands tomorrow is audited the day
/// it lands and nobody has to remember to add it.
final class AccessibilityAuditTests: XCTestCase {
    /// The smallest a control may be, in points, in either direction. Apple's
    /// own number, and the one a thumb actually needs.
    static let smallest: CGFloat = 44

    /// Controls that are smaller than that on purpose, by identifier prefix,
    /// each with the reason.
    ///
    /// Nothing is here to make the audit quiet. A control belongs here only
    /// when its hit area is larger than its drawing and the tree reports the
    /// drawing — which the system's own keyboard and the status bar both do,
    /// and neither is this app's to change.
    static let notOurs = ["UIKeyboard", "Key.", "com.apple."]

    func testEveryControlIsNamedAndBigEnoughToHit() throws {
        let port = ProcessInfo.processInfo.environment["AMUX_DOOR_PORT"] ?? "8790"
        let app = XCUIApplication()
        app.launchArguments = ["-amux-door-port", port, "-amux-scripted-cloud"]
        app.launch()

        let door = try JourneyCase.Lines(address: "127.0.0.1:\(port)")
        let answered = try door.ask(["kind": "states"])
        let states = try XCTUnwrap(
            answered["states"] as? [[String: Any]],
            "the door did not say which states this build draws: \(answered)")
        XCTAssertFalse(states.isEmpty, "the door says this build draws nothing")

        var audited = 0
        var complaints: [String] = []
        for state in states {
            guard let screen = state["screen"] as? String,
                  let name = state["state"] as? String else { continue }
            _ = try door.ask(["kind": "open", "screen": screen, "fixture": name])
            _ = try door.ask(["kind": "settle"])
            for control in controls(app) {
                audited += 1
                complaints += fault(control, on: name)
            }
        }

        XCTAssertGreaterThan(
            audited, 0, "the audit swept \(states.count) states and found no controls at all")
        record["states"] = states.count
        record["controls"] = audited
        record["faults"] = complaints
        write(record, to: "accessibility-audit.json")
        XCTAssertTrue(
            complaints.isEmpty,
            "\(complaints.count) of \(audited) controls across \(states.count) states are "
            + "unusable without sight or without a steady thumb:\n" + complaints.joined(separator: "\n"))
    }

    // MARK: - Reading the tree

    /// Everything on screen a person can press, as the accessibility client
    /// sees it.
    ///
    /// Buttons and the things that behave like them. An element with no
    /// rectangle is not on screen — a row scrolled out of a list is still in
    /// the tree — and judging one of those on its size would be judging it on
    /// nothing.
    private func controls(_ app: XCUIApplication) -> [XCUIElement] {
        let kinds: [XCUIElement.ElementType] = [.button, .switch, .link]
        return kinds.flatMap { kind in
            app.descendants(matching: kind).allElementsBoundByIndex.filter { control in
                control.exists && control.frame.width > 0 && control.frame.height > 0
                    && !Self.notOurs.contains { control.identifier.hasPrefix($0) }
            }
        }
    }

    /// What is wrong with one control, said in words a person can act on.
    private func fault(_ control: XCUIElement, on state: String) -> [String] {
        let named = control.identifier.isEmpty ? "an unnamed control" : control.identifier
        var faults: [String] = []
        if control.label.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            faults.append(
                "\(state): \(named) has nothing for VoiceOver to read out; it is at "
                + "\(rectangle(control.frame))")
        }
        let frame = control.frame
        if frame.width < Self.smallest || frame.height < Self.smallest {
            faults.append(
                "\(state): \(named) (\"\(control.label)\") is \(rectangle(frame)), under the "
                + "\(Int(Self.smallest)) pt a thumb needs")
        }
        return faults
    }

    private func rectangle(_ frame: CGRect) -> String {
        String(format: "%.0f×%.0f pt at (%.0f, %.0f)",
               frame.width, frame.height, frame.minX, frame.minY)
    }

    // MARK: - What it leaves behind

    private var record: [String: Any] = [:]

    private func write(_ record: [String: Any], to name: String) {
        let url = URL(fileURLWithPath: NSTemporaryDirectory()).appendingPathComponent(name)
        guard let data = try? JSONSerialization.data(
            withJSONObject: record, options: [.prettyPrinted, .sortedKeys]) else { return }
        try? data.write(to: url)
    }
}
