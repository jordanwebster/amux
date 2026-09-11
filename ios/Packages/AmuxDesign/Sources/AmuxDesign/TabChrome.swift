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
            .frosted(Capsule())
            .padding(.horizontal, 44)
    }
}

/// A tab's visual label, independent of the navigation action wrapping it.
/// Intrinsic symbol and text heights determine the bar's height; fixed frames
/// here would change both the label spacing and the surrounding glass geometry.
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
            Text(title)
                .font(.system(size: 10.5, weight: selected ? .semibold : .medium))
        }
        .foregroundStyle(selected ? design.ink.color : design.inkFaint.color)
        .frame(maxWidth: .infinity)
        .padding(.vertical, 9)
    }
}
