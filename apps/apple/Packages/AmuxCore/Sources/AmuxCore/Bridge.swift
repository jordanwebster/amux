import AmuxApp
import Foundation

extension Bridge {
    /// The version of the Rust library this build linked. A library built
    /// with the driving tools says so after a `+`; a release binary is
    /// checked for not carrying that suffix, so it is spelled nowhere here.
    public static var version: String {
        guard let version = amux_version() else { return "" }
        return String(cString: version)
    }
}
