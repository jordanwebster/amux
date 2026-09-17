import SwiftUI

/// How the app moves.
///
/// A static namespace rather than a field on `Design`, for the same reason
/// `Glass` is one: a skin decides colour, type and how a surface separates
/// itself from the ground, and none of those answers how long a card takes
/// to open. Nothing about a palette implies a duration.
///
/// Two curves and one tempo, because a design that moves at three unrelated
/// speeds reads as three apps. Everything that changes state uses `standard`;
/// everything that follows a reader's own scroll uses `quick`, which is
/// shorter because the content is already where the eye is; the one thing
/// that moves continuously rather than between states uses `breath`.
public enum Motion {
    /// A thing arriving, leaving or changing shape. The house curve.
    public static let standard: Animation = .snappy(duration: 0.28)

    /// Following a reader somewhere they already asked to go — a scroll to a
    /// row they pressed. Shorter than `standard`: the destination is not news.
    public static let quick: Animation = .easeOut(duration: 0.18)

    /// One leg of the working line's travel. Not a transition: this is the
    /// tempo of the only thing in the app that moves while nothing is
    /// happening, so it is slow enough to read as breathing rather than as
    /// progress.
    public static let breath: TimeInterval = 1.1
}

private struct Moving<V: Equatable>: ViewModifier {
    @Environment(\.photographed) private var photographed
    @Environment(\.reducesMotion) private var reduceMotion
    let animation: Animation
    let value: V

    func body(content: Content) -> some View {
        content.animation(still ? nil : animation, value: value)
    }

    /// A screen in front of a camera holds still for the same reason a reader
    /// who asked for less motion does: the capture keeps the last of many
    /// photographs when no run of them agree, so anything mid-flight makes a
    /// baseline a coin toss.
    private var still: Bool { photographed || reduceMotion }
}

extension View {
    /// Animate a change with one of the house curves, held still in front of a
    /// camera and for a reader who has asked for less motion.
    ///
    /// Every state change in the app should go through this rather than
    /// `.animation(_:value:)`, so that the two reasons to stop moving are
    /// decided once instead of remembered at each call site.
    public func moving<V: Equatable>(
        _ animation: Animation = Motion.standard, value: V
    ) -> some View {
        modifier(Moving(animation: animation, value: value))
    }
}
