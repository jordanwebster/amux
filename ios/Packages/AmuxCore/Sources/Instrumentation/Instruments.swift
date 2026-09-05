import AmuxCore
import Darwin
import Foundation
import QuartzCore
import UIKit

/// The display's cadence as the app sees it.
///
/// Asserted rather than assumed: a frame cap can arrive from an Info.plist
/// key, a display-link preference or a system setting, and a capped app looks
/// smooth in a measurement precisely because it is refusing to try.
public struct FrameCadence: Codable, Sendable, Equatable {
    public let maximumFramesPerSecond: Int
    public let preferredRangeLowerBound: Float
    public let preferredRangeUpperBound: Float
    public let capped: Bool
    public let disableMinimumFrameDurationOnPhone: Bool

    public init(
        maximumFramesPerSecond: Int, preferredRangeLowerBound: Float,
        preferredRangeUpperBound: Float, capped: Bool,
        disableMinimumFrameDurationOnPhone: Bool
    ) {
        self.maximumFramesPerSecond = maximumFramesPerSecond
        self.preferredRangeLowerBound = preferredRangeLowerBound
        self.preferredRangeUpperBound = preferredRangeUpperBound
        self.capped = capped
        self.disableMinimumFrameDurationOnPhone = disableMinimumFrameDurationOnPhone
    }

    /// Whether the app is asking for everything the display can give.
    public var ready: Bool {
        !capped && disableMinimumFrameDurationOnPhone
            && Int(preferredRangeUpperBound.rounded()) == maximumFramesPerSecond
    }

    @MainActor
    public static func current(in scene: UIWindowScene? = nil) -> FrameCadence {
        let screen = scene?.screen ?? UIApplication.shared.connectedScenes
            .compactMap { ($0 as? UIWindowScene)?.screen }.first
        let maximum = screen?.maximumFramesPerSecond ?? 60
        let allowed = Bundle.main
            .object(forInfoDictionaryKey: "CADisableMinimumFrameDurationOnPhone") as? Bool ?? false
        // A link created with no preference asks for the display's full range;
        // if the system hands back a lower ceiling, something is capping it.
        let link = CADisplayLink(target: Cap.shared, selector: #selector(Cap.tick))
        let range = link.preferredFrameRateRange
        link.invalidate()
        let ceiling = range.maximum > 0 ? range.maximum : Float(maximum)
        return FrameCadence(
            maximumFramesPerSecond: maximum,
            preferredRangeLowerBound: range.minimum,
            preferredRangeUpperBound: ceiling,
            capped: Int(ceiling.rounded()) < maximum || !allowed,
            disableMinimumFrameDurationOnPhone: allowed)
    }

    private final class Cap: NSObject, @unchecked Sendable {
        static let shared = Cap()
        @objc func tick() {}
    }
}

/// Missed-frame accounting over a stretch of time.
///
/// A hitch is a frame that took longer than the display gave it. Summing the
/// overruns and dividing by how long the run lasted gives the milliseconds of
/// hitch per second the streaming budget is written in. On a device this is
/// `XCTHitchMetric`'s job; on the simulator it is a proxy, because the frames
/// are composited by the Mac's display and not the phone's.
@MainActor
public final class FrameWatch {
    private var link: CADisplayLink?
    private var previous: CFTimeInterval?
    private var hitchSeconds: CFTimeInterval = 0
    private var startedAt: CFTimeInterval = 0
    public private(set) var frames = 0

    public init() {}

    public func start() {
        hitchSeconds = 0
        frames = 0
        previous = nil
        startedAt = CACurrentMediaTime()
        let link = CADisplayLink(target: self, selector: #selector(fired))
        link.add(to: .main, forMode: .common)
        self.link = link
    }

    /// Milliseconds of hitch per second of running, and the frames seen.
    @discardableResult
    public func stop() -> Double {
        link?.invalidate()
        link = nil
        let elapsed = max(CACurrentMediaTime() - startedAt, 0.001)
        return hitchSeconds * 1_000 / elapsed
    }

    @objc private func fired(_ link: CADisplayLink) {
        frames += 1
        let expected = max(link.targetTimestamp - link.timestamp, 0.001)
        if let previous {
            let actual = link.timestamp - previous
            if actual > expected { hitchSeconds += actual - expected }
        }
        previous = link.timestamp
    }
}

/// How much of one core the main thread used between two readings.
public struct CPUWatch: Sendable {
    private let thread: CFTimeInterval
    private let wall: CFTimeInterval

    public init() {
        thread = CPUWatch.mainThreadSeconds()
        wall = CACurrentMediaTime()
    }

    /// Percent of one core, averaged over the interval.
    public func percent() -> Double {
        let usedCPU = CPUWatch.mainThreadSeconds() - thread
        let elapsed = max(CACurrentMediaTime() - wall, 0.001)
        return usedCPU / elapsed * 100
    }

    /// The calling thread's user plus system time. Called from the main thread
    /// on both sides of a measurement, so it is the main thread's own cost
    /// rather than the whole process's.
    private static func mainThreadSeconds() -> CFTimeInterval {
        var info = thread_basic_info()
        // The C macro that names this size is not imported into Swift.
        var count = mach_msg_type_number_t(
            MemoryLayout<thread_basic_info_data_t>.size / MemoryLayout<integer_t>.size)
        let read = withUnsafeMutablePointer(to: &info) {
            $0.withMemoryRebound(to: integer_t.self, capacity: Int(count)) {
                thread_info(mach_thread_self(), thread_flavor_t(THREAD_BASIC_INFO), $0, &count)
            }
        }
        guard read == KERN_SUCCESS else { return 0 }
        let user = Double(info.user_time.seconds) + Double(info.user_time.microseconds) / 1e6
        let system = Double(info.system_time.seconds) + Double(info.system_time.microseconds) / 1e6
        return user + system
    }
}

/// What the process is holding, as the system accounts for it — the same
/// number a jetsam decision is made on, not the resident size.
public enum Footprint {
    public static func megabytes() -> Double {
        var info = task_vm_info_data_t()
        var count = mach_msg_type_number_t(MemoryLayout<task_vm_info_data_t>.size / MemoryLayout<integer_t>.size)
        let read = withUnsafeMutablePointer(to: &info) {
            $0.withMemoryRebound(to: integer_t.self, capacity: Int(count)) {
                task_info(mach_task_self_, task_flavor_t(TASK_VM_INFO), $0, &count)
            }
        }
        guard read == KERN_SUCCESS else { return 0 }
        return Double(info.phys_footprint) / 1_048_576
    }
}
