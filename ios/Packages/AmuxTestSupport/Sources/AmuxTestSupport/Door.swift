import AmuxDesign
import Foundation
import SwiftUI

/// What a Mac-side driver can ask a debug build to do, and what it answers.
///
/// The wire is one JSON object per line over loopback. Every case is tagged by
/// a `kind` string with its fields alongside rather than nested, so a request
/// reads the same in a Swift test, in a Rust client and in a transcript
/// somebody is trying to understand after a failure.
public enum DoorRequest: Sendable, Equatable {
    /// Show a named screen, filled from a named state. Absent means the state
    /// whose name matches the screen.
    case open(screen: String, fixture: String?)
    /// Rewrite what the cloud will answer from here on.
    case cloud(ScriptedCloudState)
    /// Start the shared runtime against a relay with a credential.
    case connect(relay: String, token: String, user: String)
    /// Wait until the fleet has been confirmed by a host, or give up after
    /// this many seconds. A connection is asynchronous, so a driver that read
    /// the screen straight after `connect` would read the moment before it.
    case awaitReconciled(seconds: Double)
    /// What library this app linked and what its connection has arrived at.
    case bridge
    case appearance(Appearance)
    case dynamicType(String)
    /// Wait until the screen has stopped changing. A capture that does not
    /// wait for this photographs a frame mid-animation.
    case settle
    case query
    /// Write a PNG of the composited window at the given path, which is the
    /// app's own to write — a driver on the Mac reads it back out of the
    /// app's container.
    case capture(path: String)
    case tap(identifier: String)
    case type(identifier: String, text: String)
    case shutdown
}

public enum DoorReply: Sendable, Equatable {
    case ack
    case state(VisibleState)
    case bridge(BridgeState)
    case captured(path: String, width: Int, height: Int, scale: Int)
    /// Why the request could not be answered, in one line. The door never
    /// half-answers: a request either happened or is reported here.
    case error(String)
}

/// What is on screen, as the accessibility tree reports it: the same elements
/// a journey drives and a person using VoiceOver hears, depth-first.
public struct VisibleState: Codable, Sendable, Equatable {
    /// The screen the door was last asked to open, or `none`.
    public let screen: String
    public let elements: [VisibleElement]
    /// Whether the fleet on screen has been confirmed by a host.
    public let reconciled: Bool
    /// How many rows are still drawn as unconfirmed.
    public let shimmering: Int

    public init(screen: String, elements: [VisibleElement], reconciled: Bool, shimmering: Int) {
        self.screen = screen
        self.elements = elements
        self.reconciled = reconciled
        self.shimmering = shimmering
    }
}

/// What the shared runtime under this app is and what it has reached.
///
/// The build is the marker the linked library answers with, so a driver can
/// tell the shipping library from the one with the driving tools compiled in
/// without guessing from behaviour. The rest is what a connection actually
/// produced: an acknowledged `connect` only means the runtime started, and
/// hosts and agents named here mean it reached the other end.
public struct BridgeState: Codable, Sendable, Equatable {
    public let build: String
    /// Whether a connection has been started at all.
    public let started: Bool
    /// The connection's own word for where it is.
    public let connection: String
    /// Whether the fleet has been confirmed by a host rather than remembered.
    public let reconciled: Bool
    /// The machines the fleet names, by name. A machine appears here once this
    /// device is paired with it; before that the fleet is confirmed and empty.
    public let hosts: [String]
    /// The agents the fleet names, by name.
    public let agents: [String]
    /// The machines the connection has seen on the other side, by name,
    /// whether or not this device is paired with them. Where the fleet is
    /// what the user may open, this is what the runtime found — the one thing
    /// that tells a connection which reached a host from one which only
    /// started.
    public let discovered: [String]

    public init(
        build: String, started: Bool, connection: String, reconciled: Bool,
        hosts: [String], agents: [String], discovered: [String]
    ) {
        self.build = build
        self.started = started
        self.connection = connection
        self.reconciled = reconciled
        self.hosts = hosts
        self.agents = agents
        self.discovered = discovered
    }
}

public struct VisibleElement: Codable, Sendable, Equatable {
    public let identifier: String
    public let label: String?
    public let value: String?
    public let frame: VisibleFrame
    public let enabled: Bool

    public init(
        identifier: String, label: String?, value: String?, frame: VisibleFrame, enabled: Bool
    ) {
        self.identifier = identifier
        self.label = label
        self.value = value
        self.frame = frame
        self.enabled = enabled
    }
}

/// A rectangle in points, in the window's coordinates.
public struct VisibleFrame: Codable, Sendable, Equatable {
    public let x: Double
    public let y: Double
    public let width: Double
    public let height: Double

    public init(x: Double, y: Double, width: Double, height: Double) {
        self.x = x
        self.y = y
        self.width = width
        self.height = height
    }
}

// MARK: - The wire

extension DoorRequest: Codable {
    private enum Key: String, CodingKey {
        case kind, screen, fixture, cloud, relay, token, user, appearance, size, path
        case identifier, text, seconds
    }

    public init(from decoder: any Decoder) throws {
        let fields = try decoder.container(keyedBy: Key.self)
        let kind = try fields.decode(String.self, forKey: .kind)
        switch kind {
        case "open":
            self = .open(
                screen: try fields.decode(String.self, forKey: .screen),
                fixture: try fields.decodeIfPresent(String.self, forKey: .fixture))
        case "cloud":
            self = .cloud(try fields.decode(ScriptedCloudState.self, forKey: .cloud))
        case "connect":
            self = .connect(
                relay: try fields.decode(String.self, forKey: .relay),
                token: try fields.decode(String.self, forKey: .token),
                user: try fields.decode(String.self, forKey: .user))
        case "awaitReconciled":
            self = .awaitReconciled(seconds: try fields.decode(Double.self, forKey: .seconds))
        case "bridge": self = .bridge
        case "appearance":
            self = .appearance(try fields.decode(Appearance.self, forKey: .appearance))
        case "dynamicType":
            self = .dynamicType(try fields.decode(String.self, forKey: .size))
        case "settle": self = .settle
        case "query": self = .query
        case "capture":
            self = .capture(path: try fields.decode(String.self, forKey: .path))
        case "tap":
            self = .tap(identifier: try fields.decode(String.self, forKey: .identifier))
        case "type":
            self = .type(
                identifier: try fields.decode(String.self, forKey: .identifier),
                text: try fields.decode(String.self, forKey: .text))
        case "shutdown": self = .shutdown
        default:
            throw DecodingError.dataCorruptedError(
                forKey: .kind, in: fields, debugDescription: "no door request named \(kind)")
        }
    }

    public func encode(to encoder: any Encoder) throws {
        var fields = encoder.container(keyedBy: Key.self)
        switch self {
        case .open(let screen, let fixture):
            try fields.encode("open", forKey: .kind)
            try fields.encode(screen, forKey: .screen)
            try fields.encodeIfPresent(fixture, forKey: .fixture)
        case .cloud(let state):
            try fields.encode("cloud", forKey: .kind)
            try fields.encode(state, forKey: .cloud)
        case .connect(let relay, let token, let user):
            try fields.encode("connect", forKey: .kind)
            try fields.encode(relay, forKey: .relay)
            try fields.encode(token, forKey: .token)
            try fields.encode(user, forKey: .user)
        case .awaitReconciled(let seconds):
            try fields.encode("awaitReconciled", forKey: .kind)
            try fields.encode(seconds, forKey: .seconds)
        case .bridge:
            try fields.encode("bridge", forKey: .kind)
        case .appearance(let appearance):
            try fields.encode("appearance", forKey: .kind)
            try fields.encode(appearance, forKey: .appearance)
        case .dynamicType(let size):
            try fields.encode("dynamicType", forKey: .kind)
            try fields.encode(size, forKey: .size)
        case .settle:
            try fields.encode("settle", forKey: .kind)
        case .query:
            try fields.encode("query", forKey: .kind)
        case .capture(let path):
            try fields.encode("capture", forKey: .kind)
            try fields.encode(path, forKey: .path)
        case .tap(let identifier):
            try fields.encode("tap", forKey: .kind)
            try fields.encode(identifier, forKey: .identifier)
        case .type(let identifier, let text):
            try fields.encode("type", forKey: .kind)
            try fields.encode(identifier, forKey: .identifier)
            try fields.encode(text, forKey: .text)
        case .shutdown:
            try fields.encode("shutdown", forKey: .kind)
        }
    }
}

extension DoorReply: Codable {
    private enum Key: String, CodingKey {
        case kind, state, bridge, path, width, height, scale, message
    }

    public init(from decoder: any Decoder) throws {
        let fields = try decoder.container(keyedBy: Key.self)
        let kind = try fields.decode(String.self, forKey: .kind)
        switch kind {
        case "ack": self = .ack
        case "state":
            self = .state(try fields.decode(VisibleState.self, forKey: .state))
        case "bridge":
            self = .bridge(try fields.decode(BridgeState.self, forKey: .bridge))
        case "captured":
            self = .captured(
                path: try fields.decode(String.self, forKey: .path),
                width: try fields.decode(Int.self, forKey: .width),
                height: try fields.decode(Int.self, forKey: .height),
                scale: try fields.decode(Int.self, forKey: .scale))
        case "error":
            self = .error(try fields.decode(String.self, forKey: .message))
        default:
            throw DecodingError.dataCorruptedError(
                forKey: .kind, in: fields, debugDescription: "no door reply named \(kind)")
        }
    }

    public func encode(to encoder: any Encoder) throws {
        var fields = encoder.container(keyedBy: Key.self)
        switch self {
        case .ack:
            try fields.encode("ack", forKey: .kind)
        case .state(let state):
            try fields.encode("state", forKey: .kind)
            try fields.encode(state, forKey: .state)
        case .bridge(let state):
            try fields.encode("bridge", forKey: .kind)
            try fields.encode(state, forKey: .bridge)
        case .captured(let path, let width, let height, let scale):
            try fields.encode("captured", forKey: .kind)
            try fields.encode(path, forKey: .path)
            try fields.encode(width, forKey: .width)
            try fields.encode(height, forKey: .height)
            try fields.encode(scale, forKey: .scale)
        case .error(let message):
            try fields.encode("error", forKey: .kind)
            try fields.encode(message, forKey: .message)
        }
    }
}

/// The launch arguments and file names the door and its driver both depend on.
public enum Door {
    /// `-amux-door-ready PATH`: where the app writes the port it is listening
    /// on, once it is listening. The driver waits for this file rather than
    /// guessing a port or a delay.
    public static let readyArgument = "amux-door-ready"

    /// What the ready file holds.
    public struct Ready: Codable, Sendable, Equatable {
        public let port: UInt16
        public let pid: Int32

        public init(port: UInt16, pid: Int32) {
            self.port = port
            self.pid = pid
        }
    }
}

extension SwiftUI.DynamicTypeSize {
    /// The door's names for the reader's type sizes. They are the plain
    /// spellings a person would write in a request or a fixture, and a fixture
    /// and a door request that name the same size get the same size.
    public init?(doorName: String) {
        switch doorName {
        case "xSmall": self = .xSmall
        case "small": self = .small
        case "medium": self = .medium
        case "large": self = .large
        case "xLarge": self = .xLarge
        case "xxLarge": self = .xxLarge
        case "xxxLarge": self = .xxxLarge
        case "accessibility1": self = .accessibility1
        case "accessibility2": self = .accessibility2
        case "accessibility3": self = .accessibility3
        case "accessibility4": self = .accessibility4
        case "accessibility5": self = .accessibility5
        default: return nil
        }
    }
}
