import Foundation
import SwiftUI
#if canImport(UIKit)
import UIKit
#endif

/// The picture of the screen a report is about, and how big it was.
///
/// The image is the composited window, not a drawing the app makes of its own
/// view tree: what somebody is complaining about includes the material, the
/// blur and the system's own status bar, none of which the app can redraw.
///
/// The size is carried beside the bytes because a rectangle drawn on the
/// picture later has to be expressed in the same unit the phone laid the
/// screen out in. Points and scale rather than pixels: a note that said
/// "1206 across" would mean a different place on a phone at a different
/// scale, and the bundle is read on a Mac that has neither.
public struct FrozenFrame: Sendable, Equatable {
    /// The composited screen, as PNG bytes.
    public var png: Data
    /// How wide the screen was, in points.
    public var width: Double
    /// How tall it was, in points.
    public var height: Double
    /// How many pixels the phone drew per point.
    public var scale: Double

    public init(png: Data, width: Double, height: Double, scale: Double) {
        self.png = png
        self.width = width
        self.height = height
        self.scale = scale
    }
}

extension FrozenFrame {
    /// The frozen screen as something a view can draw, or nothing when the
    /// bytes are not an image this phone can read.
    ///
    /// The decode lives beside the value rather than in the screen that shows
    /// it: turning phone pixels back into a picture is the platform's, and the
    /// screens in this app are written to be functions of their state with no
    /// platform in them at all.
    public var picture: Image? {
        #if canImport(UIKit)
        guard let image = UIImage(data: png) else { return nil }
        return Image(uiImage: image)
        #else
        return nil
        #endif
    }
}

/// Everything a report is made of, taken in one instant.
///
/// The three recordings are frozen together and before anything else happens,
/// which is the whole point of the flow: what is reported is what was on
/// screen when something looked wrong, not what the app looks like once the
/// person has finished describing it. A capture taken after the report UI
/// opened would be a picture of the report.
///
/// The runtime's own recording and the view-state trace are held as the text
/// that goes into the bundle rather than as parsed values. Nothing between the
/// freeze and the send needs to read them, and holding them as text means the
/// bytes written into `msgs.jsonl` and `trace.jsonl` are the bytes that were
/// frozen — a re-encode in between could differ from what the phone saw.
public struct ReportCapture: Sendable, Equatable {
    public var frame: FrozenFrame
    /// The shared runtime's frozen recording, as the JSON the bridge answered,
    /// or nothing when nothing was connected to freeze.
    public var snapshot: String?
    /// Why there is no runtime recording, when there is none. A part that is
    /// missing says why rather than disappearing.
    public var snapshotAbsent: String?
    /// The view-state recording, one JSON object per line.
    public var trace: String?
    /// Why there is no view-state recording, when there is none.
    public var traceAbsent: String?
    /// Where the person was when they froze it, by the name the screen
    /// catalogue gives it. Written into the report so a reader knows what they
    /// are looking at before they open the picture.
    public var route: String?

    public init(
        frame: FrozenFrame,
        snapshot: String? = nil,
        snapshotAbsent: String? = nil,
        trace: String? = nil,
        traceAbsent: String? = nil,
        route: String? = nil
    ) {
        self.frame = frame
        self.snapshot = snapshot
        self.snapshotAbsent = snapshotAbsent
        self.trace = trace
        self.traceAbsent = traceAbsent
        self.route = route
    }
}

/// Why a part of a report is not there.
///
/// A part that is missing carries the reason it is missing rather than
/// silently disappearing, so nobody reading a bundle mistakes a part somebody
/// withheld, or one the phone could not produce, for one that went astray.
public struct PartAbsent: Error, Sendable, Equatable {
    public let why: String

    public init(_ why: String) {
        self.why = why
    }
}

/// Freezes the screen and the recordings behind it.
///
/// A seam rather than a call, for the same reason the cloud and the store are
/// seams: taking a picture of the window is UIKit's, and a screen or a store
/// that reached for it could not be photographed or driven away from a device.
@MainActor
public protocol ReportFreezing: AnyObject {
    /// Everything a report needs, taken now, or nothing when the screen could
    /// not be photographed at all.
    func freeze() -> ReportCapture?
}

/// The one report this phone is in the middle of.
///
/// Its own store because the capture outlives the moment that made it: the
/// system's screenshot preview slides over the app immediately afterwards and
/// the app is put out of the way while somebody looks at it. Coming back has
/// to find the same frozen frame and the same offer, which it cannot if the
/// offer is view state on a screen that went away.
///
/// One at a time. A second screenshot while a report is being written replaces
/// nothing: the person is already describing the first, and swapping the
/// picture under them would lose the notes they had already put on it.
@MainActor
@Observable
public final class ReportStore {
    /// What was frozen, until it is sent or thrown away.
    public private(set) var capture: ReportCapture?
    /// Whether the app is offering to report it. False while the report itself
    /// is open — the offer has been taken by then.
    public private(set) var offering = false
    /// Whether the report is open over the app.
    public private(set) var open = false

    public init() {}

    /// Freezes the screen and offers to report it.
    ///
    /// This is the screenshot path. The offer appears only after the capture
    /// is in hand, so there is no window in which the app has drawn something
    /// about reporting and not yet taken the picture.
    @discardableResult
    public func offer(_ freezer: any ReportFreezing) -> Bool {
        guard !open, capture == nil else { return false }
        guard let frozen = freezer.freeze() else { return false }
        capture = frozen
        offering = true
        return true
    }

    /// Freezes the screen and opens the report on it, with no offer in
    /// between. This is the deliberate path, from Help: somebody who went
    /// looking for it has already said yes.
    @discardableResult
    public func begin(_ freezer: any ReportFreezing) -> Bool {
        guard !open else { return false }
        guard let frozen = freezer.freeze() else { return false }
        capture = frozen
        offering = false
        open = true
        return true
    }

    /// Took the offer up. Nothing is captured here: the picture was taken when
    /// the offer was made, which is what makes it a picture of the problem.
    public func accept() {
        guard capture != nil else { return }
        offering = false
        open = true
    }

    /// Turned the offer down, or closed the report. The capture goes with it.
    public func dismiss() {
        offering = false
        open = false
        capture = nil
    }
}
