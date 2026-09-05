import AmuxDesign
import AmuxFeatures
import Foundation

/// A named state the app can be put into without a network or a host: what
/// screen it shows and in which appearance. Fixtures are how goldens, the
/// driving door and unit tests all name the same state.
public struct Fixture: Identifiable, Sendable {
    public let id: String
    public let screen: Screen
    public let appearance: Appearance

    public init(id: String, screen: Screen, appearance: Appearance = .light) {
        self.id = id
        self.screen = screen
        self.appearance = appearance
    }
}

public enum Fixtures {
    public static let all: [Fixture] = [
        Fixture(id: "probe", screen: .probe),
    ]

    public static func named(_ id: String) -> Fixture? {
        all.first { $0.id == id }
    }
}
