import SwiftUI

/// One thing on screen, as the screen itself declares it.
public struct IdentifiedElement: Equatable, Sendable {
    public let identifier: String
    public let label: String?
    public let value: String?
    /// Where it is, in the window's coordinates.
    public let frame: CGRect
    public let enabled: Bool

    public init(
        identifier: String, label: String? = nil, value: String? = nil,
        frame: CGRect, enabled: Bool = true
    ) {
        self.identifier = identifier
        self.label = label
        self.value = value
        self.frame = frame
        self.enabled = enabled
    }
}

/// Everything the screen below has named, in the order it draws it.
public struct IdentifiedElements: PreferenceKey {
    public static let defaultValue: [IdentifiedElement] = []

    public static func reduce(
        value: inout [IdentifiedElement], nextValue: () -> [IdentifiedElement]
    ) {
        value.append(contentsOf: nextValue())
    }
}

extension View {
    /// Names something on screen once, for everybody who needs the name.
    ///
    /// It sets the accessibility identifier a journey and VoiceOver use, and
    /// it reports the same name, label, value and frame up the view tree for
    /// the driving door to read back.
    ///
    /// Two consumers and one declaration, because the alternative is a screen
    /// whose door query and whose XCUITest disagree about what is on it. The
    /// door cannot read the accessibility tree instead: SwiftUI builds that
    /// tree only for an attached accessibility client, so a query from inside
    /// the process sees nothing at all.
    public func identified(
        _ identifier: String, label: String? = nil, value: String? = nil, enabled: Bool = true
    ) -> some View {
        accessibilityIdentifier(identifier)
            .modifier(Identify(identifier: identifier, label: label, value: value, enabled: enabled))
    }
}

private struct Identify: ViewModifier {
    let identifier: String
    let label: String?
    let value: String?
    let enabled: Bool

    func body(content: Content) -> some View {
        content.background {
            GeometryReader { geometry in
                Color.clear.preference(
                    key: IdentifiedElements.self,
                    value: [IdentifiedElement(
                        identifier: identifier, label: label, value: value,
                        frame: geometry.frame(in: .global), enabled: enabled)])
            }
        }
    }
}
