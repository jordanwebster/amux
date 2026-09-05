import AmuxMobile
import Foundation

/// The shared Rust library this build is linked against. Swift never
/// reimplements protocol, projection or send-gate behaviour; it reads it here.
public enum Bridge {
    public static var version: String {
        guard let version = amux_mobile_version() else { return "" }
        return String(cString: version)
    }

    /// Which of the two libraries this binary linked. The version alone is the
    /// shipping library; the version with `+debug-tools` is the one built with
    /// the driving tools, which accepts a plaintext relay on this machine and
    /// can freeze a recorder snapshot. Reading it costs nothing and is the
    /// only way an app can tell from the inside.
    public static var build: String {
        guard let build = amux_mobile_build() else { return "" }
        return String(cString: build)
    }

    // The suffix `build` carries when the driving tools are compiled in is
    // deliberately not spelled anywhere in Swift. It is a string literal only
    // the driving library contains, and `wt run ios-door-smoke` proves a
    // release binary does not carry it by searching the bytes — a constant
    // here would be found by that search and the check would be worthless.
}
