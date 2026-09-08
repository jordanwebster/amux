import Darwin
import Foundation
import LaunchClock
import QuartzCore
import os

/// The named moments the app marks in every build.
///
/// One vocabulary serves three readers: Instruments shows these names on a
/// timeline, the performance suite measures the intervals between them, and a
/// captured report carries them beside the events that caused them. They are
/// emitted in release as well as debug, because a timing that only exists in a
/// debug build measures the debug build.
public enum Signpost: String, Sendable, CaseIterable, Codable {
    /// The kernel's start time for this process, not the first line of main().
    case processStart
    /// The moment the dynamic linker had finished mapping, binding and
    /// initialising every image the app is built out of, just before main().
    ///
    /// Everything before it is loading the app; everything after it is
    /// running it. The two get slower for completely different reasons — one
    /// from what the binary is made of, the other from what the code does —
    /// so a launch that got slower is worth almost nothing as a single
    /// number and quite a lot once it is cut here.
    case imagesLoaded
    /// The first moment this app's own code runs.
    ///
    /// Between `imagesLoaded` and this one the app is loaded but idle: UIKit
    /// is starting up and getting as far as building the scene that asks for
    /// the app's first view.
    case appEntered
    /// The first frame the display has actually shown carrying cached rows.
    case firstCachedFrame
    case streamConnected
    case reconciled
    /// The instant a send is handled, before anything has been drawn or has
    /// left the phone.
    case sendTapped
    /// The first frame the display has actually shown carrying the row the
    /// person just sent. Drawn from what was typed rather than from anything
    /// the host said, so the interval from `sendTapped` is this app's own
    /// work and never the network's.
    case echoCommitted
    case streamRow
    case transcriptCommit
    /// One display refresh the app asked for. An idle app asks for none.
    case idleTick
}

/// One emission: the signpost and when it happened.
public struct SignpostMark: Sendable, Equatable, Codable {
    public let signpost: Signpost
    /// Seconds since the kernel started this process, so two marks can be
    /// subtracted without agreeing on a wall clock.
    public let sinceProcessStart: Double

    public init(signpost: Signpost, sinceProcessStart: Double) {
        self.signpost = signpost
        self.sinceProcessStart = sinceProcessStart
    }
}

/// Where a signpost goes: to the system's own tracing, and to a small journal
/// the app can read back.
///
/// The journal exists because the measurements this app is held to — cold
/// first frame, reconciliation, idle quiet — are intervals between two marks,
/// and reading them back in-process is what lets a test assert a number
/// instead of a person reading a trace.
public enum Signposts {
    public static let subsystem = "sh.amux.Amux"

    private static let log = OSLog(subsystem: subsystem, category: "performance")
    private static let journal = Journal()

    /// When the kernel started this process, as a monotonic reference. Taken
    /// from the process table rather than from a static initialiser, so the
    /// dynamic linker's work before any of this code ran is inside the
    /// measurement rather than hidden by it.
    public static let processStartedAt: Date = kernelProcessStart() ?? Date()

    /// Seconds from the kernel starting this process to the dynamic linker
    /// finishing with it, or nothing if the image initialiser never ran.
    ///
    /// Both ends are absolute, so this reads the same whenever it is asked.
    public static let imagesLoadedAfter: Double? = {
        let loadedAt = amux_images_loaded_at()
        guard loadedAt != 0 else { return nil }
        var timebase = mach_timebase_info_data_t()
        guard mach_timebase_info(&timebase) == KERN_SUCCESS, timebase.denom != 0 else {
            return nil
        }
        let ticks = mach_absolute_time() - loadedAt
        let sinceLoaded =
            Double(ticks) * Double(timebase.numer) / Double(timebase.denom) / 1_000_000_000
        return Date().timeIntervalSince(processStartedAt) - sinceLoaded
    }()

    /// Marks a moment now.
    @discardableResult
    public static func emit(_ signpost: Signpost) -> SignpostMark {
        os_signpost(.event, log: log, name: "amux", "%{public}s", signpost.rawValue)
        let mark = SignpostMark(
            signpost: signpost,
            sinceProcessStart: Date().timeIntervalSince(processStartedAt))
        journal.append(mark)
        return mark
    }

    /// Marks a moment once the display has presented the frame the caller is
    /// about to cause. A mark taken when state changes measures the state
    /// change; this one measures what a person saw.
    public static func emitWhenPresented(_ signpost: Signpost) {
        Presentation.after { emit(signpost) }
    }

    /// Marks a moment once the frame the caller is about to cause has been
    /// committed for display, without waiting for the refresh after it.
    ///
    /// The difference between this and `emitWhenPresented` is one display
    /// refresh, deliberately: waiting for the refresh after the commit is
    /// slack worth having when the answer is four hundred milliseconds long
    /// and nobody can tell one frame from the next. It is not worth having
    /// when the whole budget is one frame, because the slack is then half the
    /// number and the instrument reports two frames for work that took one.
    public static func emitWhenDrawn(_ signpost: Signpost) {
        Presentation.committed { emit(signpost) }
    }

    /// Every mark so far, oldest first.
    public static var marks: [SignpostMark] { journal.marks }

    /// The first time this signpost was marked, in seconds since the process
    /// started, or nothing if it has not been marked.
    public static func first(_ signpost: Signpost) -> Double? {
        journal.marks.first { $0.signpost == signpost }?.sinceProcessStart
    }

    public static func count(_ signpost: Signpost) -> Int {
        journal.marks.filter { $0.signpost == signpost }.count
    }

    /// Forgets everything marked so far. The performance suite resets between
    /// samples so one sample cannot read the previous one's marks.
    public static func reset() {
        journal.reset()
    }

    private final class Journal: @unchecked Sendable {
        /// Enough for a whole measurement run and bounded, so a long-running
        /// app cannot grow a list of marks without limit.
        private static let limit = 65_536
        private let lock = NSLock()
        /// The process's own start is mark zero: every other mark is stated
        /// as a distance from it, so the journal is readable on its own. The
        /// linker's finish is the other mark nobody emits, because it
        /// happened before there was anything to emit it.
        private var kept: [SignpostMark] = Journal.origin()

        private static func origin() -> [SignpostMark] {
            var marks = [SignpostMark(signpost: .processStart, sinceProcessStart: 0)]
            if let loaded = Signposts.imagesLoadedAfter {
                marks.append(SignpostMark(signpost: .imagesLoaded, sinceProcessStart: loaded))
            }
            return marks
        }

        var marks: [SignpostMark] { lock.withLock { kept } }

        func append(_ mark: SignpostMark) {
            lock.withLock {
                if kept.count == Self.limit { kept.removeFirst() }
                kept.append(mark)
            }
        }

        func reset() {
            lock.withLock {
                kept = Journal.origin()
            }
        }
    }

    private static func kernelProcessStart() -> Date? {
        var info = kinfo_proc()
        var size = MemoryLayout<kinfo_proc>.stride
        var name: [Int32] = [CTL_KERN, KERN_PROC, KERN_PROC_PID, getpid()]
        let read = sysctl(&name, UInt32(name.count), &info, &size, nil, 0)
        guard read == 0 else { return nil }
        let started = info.kp_proc.p_starttime
        return Date(
            timeIntervalSince1970: Double(started.tv_sec) + Double(started.tv_usec) / 1_000_000)
    }
}

/// A callback that runs after the frame being assembled has been presented.
///
/// A Core Animation transaction completion runs when the render server has
/// committed the frame, and one further display refresh means it is on screen.
enum Presentation {
    static func after(_ body: @escaping @Sendable () -> Void) {
        committed {
            MainActor.assumeIsolated {
                DisplayTick.once { body() }
            }
        }
    }

    /// Runs the body when the render server has committed the frame being
    /// assembled — the frame that carries whatever the caller just changed.
    static func committed(_ body: @escaping @Sendable () -> Void) {
        CATransaction.begin()
        CATransaction.setCompletionBlock { body() }
        CATransaction.commit()
    }
}

/// Every display refresh the app asks for goes through here.
///
/// The idle budget is stated as display refreshes requested, so an animation
/// that drives itself with its own `CADisplayLink` outside this type would be
/// invisible to the measurement that is supposed to catch it. Screens animate
/// through this, and the suite counts what it emitted.
@MainActor
public enum DisplayTick {
    private static var waiters: [Waiter] = []

    /// Runs the body on the next display refresh.
    public static func once(_ body: @escaping @MainActor () -> Void) {
        let waiter = Waiter(body: body)
        waiters.append(waiter)
        waiter.start()
    }

    /// Runs the body on every display refresh until the returned handle is
    /// released or cancelled.
    public static func repeating(_ body: @escaping @MainActor () -> Void) -> Ticker {
        Ticker(body: body)
    }

    fileprivate static func finished(_ waiter: Waiter) {
        waiters.removeAll { $0 === waiter }
    }

    @MainActor
    fileprivate final class Waiter {
        private var link: CADisplayLink?
        private let body: @MainActor () -> Void

        init(body: @escaping @MainActor () -> Void) {
            self.body = body
        }

        func start() {
            let link = CADisplayLink(target: self, selector: #selector(fired))
            link.add(to: .main, forMode: .common)
            self.link = link
        }

        @objc private func fired() {
            Signposts.emit(.idleTick)
            link?.invalidate()
            link = nil
            body()
            DisplayTick.finished(self)
        }
    }

    /// A running animation's refreshes. The display link holds this object
    /// while it runs, so an animation stops when it is cancelled rather than
    /// when the last reference to it goes; forgetting to cancel one is a
    /// screen still asking for frames, which is exactly what the idle budget
    /// is there to catch.
    @MainActor
    public final class Ticker {
        private var link: CADisplayLink?
        private let body: @MainActor () -> Void

        fileprivate init(body: @escaping @MainActor () -> Void) {
            self.body = body
            let link = CADisplayLink(target: self, selector: #selector(fired))
            link.add(to: .main, forMode: .common)
            self.link = link
        }

        public func cancel() {
            link?.invalidate()
            link = nil
        }

        @objc private func fired() {
            Signposts.emit(.idleTick)
            body()
        }
    }
}
