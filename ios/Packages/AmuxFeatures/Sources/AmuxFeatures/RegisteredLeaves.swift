import Foundation

/// The UIKit views this app is allowed to contain.
///
/// Everything else in this package is SwiftUI and platform-neutral, so a
/// screen is a function of its state and can be captured, replayed and — when
/// there is a Mac app — drawn again without being rewritten. A UIKit view is
/// the exception, and an exception is only worth having where a measurement
/// says SwiftUI cannot do the job: a transcript that scrolls at the display's
/// cadence, a field that lays out attachment tokens inline, a diff that
/// selects a range of lines under a finger.
///
/// A leaf is one file under `Leaves/`, wrapped in one representable, named
/// here, and justified by a written measurement. `ios/Tools/feature-lint.sh`
/// refuses UIKit anywhere else in this package, so the list here is the whole
/// of it and nobody has to go looking.
///
/// A case with no file behind it is a candidate, not a leaf: it is what the
/// measurement is about to be taken for.
public enum RegisteredLeaves: String, CaseIterable, Sendable {
    case transcriptList
    case tokenTextField
    case diffSelection
}
