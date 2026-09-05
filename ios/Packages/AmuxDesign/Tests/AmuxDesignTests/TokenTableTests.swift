import Foundation
import SwiftUI
import XCTest
@testable import AmuxDesign

/// The whole design, rendered as text and pinned.
///
/// A token table is the one place a nudge to a colour, a radius or a type role
/// shows up as a reviewable line rather than as a pixel difference in a
/// screenshot nobody can read a diff of. Both appearances are in the table,
/// because the point of the neutral ramp is that they move together.
final class TokenTableTests: XCTestCase {
    func testTheResolvedTokenTableIsUnchanged() throws {
        let expected = try XCTUnwrap(
            Bundle.module.url(forResource: "TokenTable", withExtension: "txt"),
            "TokenTable.txt is missing from the test bundle")
        let table = Self.table(Design.app)
        if table != (try String(contentsOf: expected, encoding: .utf8)) {
            Self.writeForReview(table)
            XCTFail("The design's token table changed. The new table:\n\n\(table)")
        }
    }

    func testEveryTypeRoleScalesWithTheReadersContentSize() {
        for role in Design.Role.allCases {
            let spec = Design.app.spec(role)
            XCTAssertFalse(spec.family.isEmpty, "\(role) has no face")
            XCTAssertGreaterThan(spec.size, 0, "\(role) has no size")
            // A role set at a fixed point size would ignore Dynamic Type; every
            // one of them names the text style its size scales with.
            XCTAssertNotNil(Font.TextStyle.allCases.firstIndex(of: spec.relativeTo))
        }
    }

    func testBothAppearancesResolveEveryColourToken() {
        for (name, ramp) in Design.app.colours {
            XCTAssertEqual(ramp.hex(.light), ramp.light, "\(name) light")
            XCTAssertEqual(ramp.hex(.dark), ramp.dark, "\(name) dark")
        }
    }

    // MARK: - The table

    static func table(_ design: Design) -> String {
        var lines = [
            "design \(design.name)",
            "separation \(design.surfaces.separation.rawValue) graduated \(design.surfaces.graduated)",
            "",
            "colour           light    dark",
        ]
        for (name, ramp) in design.colours {
            lines.append("\(pad(name, 16))#\(hex(ramp.light))  #\(hex(ramp.dark))")
        }
        let metrics = design.metrics
        lines += [
            "",
            "metric           value",
            "\(pad("cardRadius", 16))\(number(metrics.cardRadius))",
            "\(pad("controlRadius", 16))\(number(metrics.controlRadius))",
            "\(pad("floatRadius", 16))\(number(metrics.floatRadius))",
            "\(pad("rowPadding", 16))\(number(metrics.rowPadding))",
            "\(pad("gutter", 16))\(number(metrics.gutter))",
            "\(pad("rowGap", 16))\(number(metrics.rowGap))",
            "\(pad("feedGap", 16))\(number(metrics.feedGap))",
            "\(pad("hairline", 16))\(number(metrics.hairline))",
            "\(pad("glassWash", 16))\(number(Glass.wash))",
            "\(pad("glassOpenWash", 16))\(number(Glass.openWash))",
            "",
            "role             face                size  weight    scales with",
        ]
        for role in Design.Role.allCases {
            let spec = design.spec(role)
            lines.append(
                pad(role.rawValue, 16)
                + pad(spec.family, 20)
                + pad(number(spec.size), 6)
                + pad(weight(spec.weight), 10)
                + "\(spec.relativeTo)"
                + (spec.tracking == 0 ? "" : "  tracking \(number(spec.tracking))"))
        }
        return lines.joined(separator: "\n") + "\n"
    }

    private static func pad(_ text: String, _ width: Int) -> String {
        text.count >= width ? text + " " : text + String(repeating: " ", count: width - text.count)
    }

    private static func hex(_ value: UInt32) -> String {
        String(format: "%06X", value)
    }

    private static func number(_ value: some BinaryFloatingPoint) -> String {
        let double = Double(value)
        return double == double.rounded()
            ? String(Int(double))
            : String(format: "%g", double)
    }

    private static func weight(_ weight: Font.Weight) -> String {
        switch weight {
        case .ultraLight: "ultraLight"
        case .thin: "thin"
        case .light: "light"
        case .regular: "regular"
        case .medium: "medium"
        case .semibold: "semibold"
        case .bold: "bold"
        case .heavy: "heavy"
        case .black: "black"
        default: "unknown"
        }
    }

    /// Writes the new table beside this file when the source tree is reachable,
    /// so a deliberate change is reviewed as a diff rather than retyped.
    private static func writeForReview(_ table: String) {
        let destination = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent()
            .appendingPathComponent("TokenTable.txt")
        try? table.write(to: destination, atomically: true, encoding: .utf8)
    }
}
