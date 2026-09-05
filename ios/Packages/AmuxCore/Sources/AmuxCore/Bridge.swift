import AmuxMobile
import Foundation

/// The shared Rust library this build is linked against. Swift never
/// reimplements protocol, projection or send-gate behaviour; it reads it here.
public enum Bridge {
    public static var version: String {
        guard let version = amux_mobile_version() else { return "" }
        return String(cString: version)
    }
}
