import SwiftUI

extension View {
    /// Grows the area a thumb can hit, around something drawn smaller than the
    /// 44 pt a finger needs.
    ///
    /// Plenty of controls are meant to be small: a close cross, a back chevron,
    /// a row of two-word choices. Drawing them at 44 pt would be a different
    /// design. What has to be 44 pt is what answers to a thumb, and those are
    /// not the same rectangle.
    ///
    /// This is deliberately half of a pair. The growth has to happen inside a
    /// `Button`'s label, because padding or a content shape applied to a button
    /// from outside does not extend what the button answers to. So this goes on
    /// the label and ``reclaimingThumbTarget(x:y:)`` goes on the button with the
    /// same numbers, and the second gives the layout back the room the first
    /// took — leaving the screen drawn exactly where it was, with a target a
    /// person can actually hit.
    ///
    /// The numbers are the growth on each side, not the finished size, so a
    /// control drawn 20 pt tall asks for `y: 12`.
    public func thumbTarget(x: CGFloat = 0, y: CGFloat = 0) -> some View {
        padding(.horizontal, x)
            .padding(.vertical, y)
            .contentShape(Rectangle())
    }

    /// The other half of ``thumbTarget(x:y:)``: takes back, from the layout,
    /// the room the thumb target took, so nothing on the screen moves.
    ///
    /// It goes outside the button and outside whatever names the button, so
    /// what is measured — by an accessibility client, and by the screen's own
    /// declaration — is the grown rectangle rather than the drawn one.
    public func reclaimingThumbTarget(x: CGFloat = 0, y: CGFloat = 0) -> some View {
        padding(.horizontal, -x)
            .padding(.vertical, -y)
    }
}
