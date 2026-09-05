import Foundation

/// One neutral ramp, with every surface role defined as a distance along it
/// rather than as a colour of its own.
///
/// Choosing distances instead of colours is what keeps light and dark reading
/// as two appearances of one design: a card lifted two steps from its ground
/// separates by the same apparent amount in both, however the ramp is nudged.
/// The scale is very slightly cool, because a true grey reads green on OLED
/// and this app is mostly neutral surface.
public enum Neutral {
    /// Index 0 is white, index 13 near-black. The steps are spaced for even
    /// apparent lightness, so they are deliberately uneven in sRGB.
    public static let scale: [UInt32] = [
        0xFFFFFF,  //  0
        0xF7F8FA,  //  1
        0xEFF1F5,  //  2
        0xE4E7ED,  //  3
        0xD3D8E1,  //  4
        0xB5BCC8,  //  5
        0x949CAB,  //  6
        0x747D8D,  //  7
        0x575F6E,  //  8
        0x3C4350,  //  9
        0x272D37,  // 10
        0x181C23,  // 11
        0x0D1015,  // 12
        0x06080B,  // 13
    ]

    public static func step(_ index: Int) -> UInt32 {
        scale[min(max(index, 0), scale.count - 1)]
    }

    /// A ramp built from a light index and its dark counterpart.
    public static func pair(light: Int, dark: Int) -> Ramp {
        Ramp(step(light), step(dark))
    }
}
