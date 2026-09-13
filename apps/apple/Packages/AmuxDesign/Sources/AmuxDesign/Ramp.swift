import SwiftUI

/// One token, stated once for both appearances and resolved where it is drawn.
///
/// The hexadecimal values stay on the token so the whole table can be read and
/// pinned as data; the `Color` is only how it reaches a view.
public struct Ramp: Sendable, Equatable {
    public let light: UInt32
    public let dark: UInt32

    public init(_ light: UInt32, _ dark: UInt32) {
        self.light = light
        self.dark = dark
    }

    public func hex(_ appearance: Appearance) -> UInt32 {
        switch appearance {
        case .light: light
        case .dark: dark
        }
    }

    /// Resolves against whatever appearance the view is drawn in.
    public var color: Color {
        Color(uiColor: UIColor { traits in
            UIColor(rgb: traits.userInterfaceStyle == .dark ? dark : light)
        })
    }

    public func color(_ appearance: Appearance) -> Color {
        Color(rgb: hex(appearance))
    }

    /// The same token as a platform colour, resolved by value.
    ///
    /// For the one view in the app drawn in UIKit, where a colour has to be a
    /// concrete one: text drawn into a picture cannot resolve a dynamic colour
    /// later, so it is resolved against the appearance it is being drawn for.
    public func uiColor(_ appearance: Appearance) -> UIColor {
        UIColor(rgb: hex(appearance))
    }
}

extension Color {
    init(rgb: UInt32) {
        self.init(
            .sRGB,
            red: Double((rgb >> 16) & 0xFF) / 255,
            green: Double((rgb >> 8) & 0xFF) / 255,
            blue: Double(rgb & 0xFF) / 255,
            opacity: 1)
    }
}

extension UIColor {
    convenience init(rgb: UInt32) {
        self.init(
            red: CGFloat((rgb >> 16) & 0xFF) / 255,
            green: CGFloat((rgb >> 8) & 0xFF) / 255,
            blue: CGFloat(rgb & 0xFF) / 255,
            alpha: 1)
    }
}
