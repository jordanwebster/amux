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
    /// Measured and answered: the SwiftUI list meets the streaming budget with
    /// room to spare, so this stays a candidate rather than becoming a leaf.
    /// `docs/IOS.md` has the numbers and what would reopen the question.
    case transcriptList
    case tokenTextField
    /// Measured by argument rather than by a workload, and answered the same
    /// way: rows report their own frames as they lay out and a drag looks a
    /// point up among them, so nothing here needs a second layout system.
    /// `docs/IOS.md` has the reasoning and what would reopen it.
    case diffSelection
}
