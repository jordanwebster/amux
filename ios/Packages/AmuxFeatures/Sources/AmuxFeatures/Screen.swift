import Foundation

/// Every screen the app can be asked to show by name, by a golden run or by a
/// journey. Screens are added here as they are built.
public enum Screen: String, Sendable, CaseIterable, Codable {
    case probe
}
