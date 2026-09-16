import SwiftUI

/// The primary navigation surface. Its caller owns selection, actions and
/// placement relative to the system's safe area; the chrome owns its layout.
public struct TabChrome<Items: View>: View {
    private let items: Items

    public init(@ViewBuilder items: () -> Items) {
        self.items = items()
    }

    public var body: some View {
        HStack(spacing: 2) { items }
            .padding(.horizontal, 6)
            .frosted(Capsule(), as: .control)
            .padding(.horizontal, 44)
    }
}

/// A tab's visual label, independent of the navigation action wrapping it.
/// The source's three intrinsic symbols happen to be nearly the same height,
/// but changing weight on a real SF Symbol changes its metrics. Stable slots
/// keep selection from moving the shared navigation surface under a finger.
public struct TabLabel: View {
    @Environment(\.design) private var design
    private let title: String
    private let glyph: String
    private let selected: Bool

    public init(_ title: String, glyph: String, selected: Bool) {
        self.title = title
        self.glyph = glyph
        self.selected = selected
    }

    public var body: some View {
        VStack(spacing: 3) {
            Image(systemName: glyph)
                .font(.system(size: 17, weight: selected ? .semibold : .regular))
                .frame(height: 21)
            Text(title)
                .font(.system(size: 10.5, weight: selected ? .semibold : .medium))
                .frame(height: 15)
        }
        .foregroundStyle(selected ? design.ink.color : design.inkFaint.color)
        .frame(maxWidth: .infinity)
        .padding(.vertical, 9)
    }
}
