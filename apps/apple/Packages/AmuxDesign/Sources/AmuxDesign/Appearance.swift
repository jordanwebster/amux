import SwiftUI

/// The two appearances every screen is drawn and captured in.
public enum Appearance: String, Sendable, CaseIterable, Codable {
    case light
    case dark

    public var colorScheme: ColorScheme {
        switch self {
        case .light: .light
        case .dark: .dark
        }
    }
}
