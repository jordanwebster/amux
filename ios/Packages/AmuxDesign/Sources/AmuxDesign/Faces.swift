import CoreText
import Foundation

/// The typefaces the app is set in.
///
/// The system face is the single biggest reason an app reads as an iOS app
/// rather than as itself, so both faces are bundled and open-licensed:
/// Instrument Sans, a grotesk with enough character to be noticed and not
/// enough to be tiring, and Geist Mono, which is what an identifier or a
/// command should look like.
public struct Faces: Sendable, Equatable {
    public let display: String
    public let body: String
    public let mono: String

    public static let instrument = Faces(
        display: "Instrument Sans", body: "Instrument Sans", mono: "Geist Mono")
}

/// The bundled font files and their registration.
///
/// The faces travel in this package's resource bundle rather than the app
/// bundle, so nothing outside can list them in `UIAppFonts`; they are
/// registered with Core Text at first use instead. Registration is idempotent:
/// a face already registered by an earlier call is not an error.
public enum BundledFonts {
    public static let files = ["InstrumentSans.ttf", "GeistMono.ttf"]

    public static func url(_ file: String) -> URL? {
        Bundle.module.url(forResource: "Fonts/\(file)", withExtension: nil)
    }

    private static let registration: Bool = {
        var registered = true
        for file in files {
            guard let url = url(file) else {
                registered = false
                continue
            }
            var error: Unmanaged<CFError>?
            if !CTFontManagerRegisterFontsForURL(url as CFURL, .process, &error) {
                // A face this process already registered is not a failure.
                let code = error.map { CFErrorGetCode($0.takeRetainedValue()) }
                registered = registered
                    && code == CTFontManagerError.alreadyRegistered.rawValue
            }
        }
        return registered
    }()

    /// Registers the bundled faces once per process and reports whether every
    /// one of them is now available.
    @discardableResult
    public static func register() -> Bool { registration }

    /// Whether Core Text can see a family by name, after registration.
    public static func isAvailable(_ family: String) -> Bool {
        register()
        let families = CTFontManagerCopyAvailableFontFamilyNames() as? [String] ?? []
        return families.contains(family)
    }
}
