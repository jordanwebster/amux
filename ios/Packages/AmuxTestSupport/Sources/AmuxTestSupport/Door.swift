import AmuxCore
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
    /// Wait until the connection has reported itself gone, or give up after
    /// this many seconds. A relay that has stopped answering is discovered by
    /// a connection failing, not by anything the driver did, so there is a
    /// moment to wait for here too.
    case awaitOffline(seconds: Double)
    /// What library this app linked and what its connection has arrived at.
    case bridge
    /// Every moment this launch has marked, in order. A driver reads them to
    /// tell a screen that was drawn from a frame that was shown, and to see
    /// when the fleet stopped being a memory.
    case signposts
    case appearance(Appearance)
    case dynamicType(String)
    /// Move one named colour token, or put it back when nothing is named.
    /// The one thing a driver can ask for that makes the app draw something
    /// its baseline does not show.
    case perturb(token: String?)
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
    /// Trust the host a pairing payload names, over the relay this app is
    /// already connected to.
    ///
    /// A person pairs a phone by reading a code off a machine, on a screen
    /// that names the host and its fingerprint before anything is written. A
    /// driver proving what a paired phone shows needs the trust rather than
    /// the ceremony, and needs it before that screen exists.
    case pair(qr: String)
    /// Ask the host holding an agent for the changes its working tree has
    /// against a base — a branch or a commit, or the working tree itself when
    /// the base is empty. The host computes the diff; the phone draws it.
    case requestChanges(agent: String, base: String)
    /// Open an agent's conversation without going to it, which is what tells
    /// the runtime this client is watching that agent.
    ///
    /// Opening one by pressing a row is a tap, and a tap takes long enough
    /// that the layer behind it has already caught up by the time the next
    /// thing happens. A driver asking what a conversation does in the moment
    /// it opens has to be there in that moment.
    case watch(agent: String)
    /// Wait until an agent's conversation will take a message, or give up.
    ///
    /// A machine that has just come back has not finished saying so: the
    /// connection returns before the layer behind it does. A driver that sent
    /// straight after a reconnection would be asking about the gap rather than
    /// about the send.
    case awaitSendable(agent: String, seconds: Double)
    /// Try to send a message to an agent, exactly as pressing send will.
    ///
    /// The answer says whether it left the phone. A refusal is not an error:
    /// refusing to send is a thing this app does deliberately, and a driver
    /// asks for it on purpose.
    case send(agent: String, text: String)
    /// Say something beside the review a conversation is already holding, and
    /// send the two together.
    ///
    /// The composer is not built, so there is nowhere on screen to write the
    /// remark that goes with a review. This puts the words where the composer
    /// will keep them and sends the draft the conversation holds — the same
    /// message, through the same gate, with the patch attached.
    case sendDraft(agent: String, prose: String)
    /// Write a report bundle into this directory: the shared runtime's own
    /// recording and the view-state trace beside it.
    case report(path: String)
    /// Rebuild the stores and the view from the bundle in this directory,
    /// without carrying out anything the recording asked the app to do.
    case replay(path: String)
    case shutdown
}

public enum DoorReply: Sendable, Equatable {
    case ack
    case state(VisibleState)
    case bridge(BridgeState)
    case signposts([SignpostMark])
    case captured(path: String, width: Int, height: Int, scale: Int)
    /// A bundle was written at this path, holding these files.
    case bundle(path: String, parts: [String])
    /// This phone now trusts the machine of this name.
    case paired(host: String)
    /// What became of an attempted send: whether it left the phone, and the
    /// sentence on screen when it did not.
    case sendAttempt(delivered: Bool, reason: String?)
    case replayed(ReplayedState)
    /// Why the request could not be answered, in one line. The door never
    /// half-answers: a request either happened or is reported here.
    case error(String)
}

/// What a bundle rebuilt: the fleet the recording held, the conversations it
/// held, and what the view-state trace then did to the screen.
///
/// A capture alone cannot show that a replay read the recording rather than a
/// fixture — the screen would look the same either way until every screen is
/// built. This says what came out of the bundle, so a driver can check the
/// rebuilt fleet against the one the bundle recorded.
public struct ReplayedState: Codable, Sendable, Equatable {
    /// Event batches the recording projected into the stores.
    public let events: Int
    /// The agents the rebuilt fleet names, by name.
    public let agents: [String]
    /// The machines the rebuilt fleet names, by name.
    public let hosts: [String]
    /// How many transcript entries each rebuilt conversation holds, by agent.
    public let entries: [String: Int]
    /// Whether the rebuilt fleet was confirmed by a host when it was recorded.
    public let reconciled: Bool
    /// View-state events applied after the stores were rebuilt.
    public let trace: Int
    /// The screen the trace left showing.
    public let screen: String

    public init(
        events: Int, agents: [String], hosts: [String], entries: [String: Int],
        reconciled: Bool, trace: Int, screen: String
    ) {
        self.events = events
        self.agents = agents
        self.hosts = hosts
        self.entries = entries
        self.reconciled = reconciled
        self.trace = trace
        self.screen = screen
    }
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
        case identifier, text, seconds, qr, agent, base, prose
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
        case "awaitOffline":
            self = .awaitOffline(seconds: try fields.decode(Double.self, forKey: .seconds))
        case "bridge": self = .bridge
        case "signposts": self = .signposts
        case "appearance":
            self = .appearance(try fields.decode(Appearance.self, forKey: .appearance))
        case "dynamicType":
            self = .dynamicType(try fields.decode(String.self, forKey: .size))
        case "perturb":
            self = .perturb(token: try fields.decodeIfPresent(String.self, forKey: .token))
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
        case "pair":
            self = .pair(qr: try fields.decode(String.self, forKey: .qr))
        case "requestChanges":
            self = .requestChanges(
                agent: try fields.decode(String.self, forKey: .agent),
                base: try fields.decode(String.self, forKey: .base))
        case "watch":
            self = .watch(agent: try fields.decode(String.self, forKey: .agent))
        case "awaitSendable":
            self = .awaitSendable(
                agent: try fields.decode(String.self, forKey: .agent),
                seconds: try fields.decode(Double.self, forKey: .seconds))
        case "send":
            self = .send(
                agent: try fields.decode(String.self, forKey: .agent),
                text: try fields.decode(String.self, forKey: .text))
        case "sendDraft":
            self = .sendDraft(
                agent: try fields.decode(String.self, forKey: .agent),
                prose: try fields.decode(String.self, forKey: .prose))
        case "report":
            self = .report(path: try fields.decode(String.self, forKey: .path))
        case "replay":
            self = .replay(path: try fields.decode(String.self, forKey: .path))
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
        case .awaitOffline(let seconds):
            try fields.encode("awaitOffline", forKey: .kind)
            try fields.encode(seconds, forKey: .seconds)
        case .bridge:
            try fields.encode("bridge", forKey: .kind)
        case .signposts:
            try fields.encode("signposts", forKey: .kind)
        case .appearance(let appearance):
            try fields.encode("appearance", forKey: .kind)
            try fields.encode(appearance, forKey: .appearance)
        case .dynamicType(let size):
            try fields.encode("dynamicType", forKey: .kind)
            try fields.encode(size, forKey: .size)
        case .perturb(let token):
            try fields.encode("perturb", forKey: .kind)
            try fields.encodeIfPresent(token, forKey: .token)
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
        case .pair(let qr):
            try fields.encode("pair", forKey: .kind)
            try fields.encode(qr, forKey: .qr)
        case .requestChanges(let agent, let base):
            try fields.encode("requestChanges", forKey: .kind)
            try fields.encode(agent, forKey: .agent)
            try fields.encode(base, forKey: .base)
        case .watch(let agent):
            try fields.encode("watch", forKey: .kind)
            try fields.encode(agent, forKey: .agent)
        case .awaitSendable(let agent, let seconds):
            try fields.encode("awaitSendable", forKey: .kind)
            try fields.encode(agent, forKey: .agent)
            try fields.encode(seconds, forKey: .seconds)
        case .send(let agent, let text):
            try fields.encode("send", forKey: .kind)
            try fields.encode(agent, forKey: .agent)
            try fields.encode(text, forKey: .text)
        case .sendDraft(let agent, let prose):
            try fields.encode("sendDraft", forKey: .kind)
            try fields.encode(agent, forKey: .agent)
            try fields.encode(prose, forKey: .prose)
        case .report(let path):
            try fields.encode("report", forKey: .kind)
            try fields.encode(path, forKey: .path)
        case .replay(let path):
            try fields.encode("replay", forKey: .kind)
            try fields.encode(path, forKey: .path)
        case .shutdown:
            try fields.encode("shutdown", forKey: .kind)
        }
    }
}

extension DoorReply: Codable {
    private enum Key: String, CodingKey {
        case kind, state, bridge, path, width, height, scale, message, parts, replayed, marks
        case host, delivered, reason
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
        case "signposts":
            self = .signposts(try fields.decode([SignpostMark].self, forKey: .marks))
        case "captured":
            self = .captured(
                path: try fields.decode(String.self, forKey: .path),
                width: try fields.decode(Int.self, forKey: .width),
                height: try fields.decode(Int.self, forKey: .height),
                scale: try fields.decode(Int.self, forKey: .scale))
        case "bundle":
            self = .bundle(
                path: try fields.decode(String.self, forKey: .path),
                parts: try fields.decode([String].self, forKey: .parts))
        case "paired":
            self = .paired(host: try fields.decode(String.self, forKey: .host))
        case "sendAttempt":
            self = .sendAttempt(
                delivered: try fields.decode(Bool.self, forKey: .delivered),
                reason: try fields.decodeIfPresent(String.self, forKey: .reason))
        case "replayed":
            self = .replayed(try fields.decode(ReplayedState.self, forKey: .replayed))
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
        case .signposts(let marks):
            try fields.encode("signposts", forKey: .kind)
            try fields.encode(marks, forKey: .marks)
        case .captured(let path, let width, let height, let scale):
            try fields.encode("captured", forKey: .kind)
            try fields.encode(path, forKey: .path)
            try fields.encode(width, forKey: .width)
            try fields.encode(height, forKey: .height)
            try fields.encode(scale, forKey: .scale)
        case .bundle(let path, let parts):
            try fields.encode("bundle", forKey: .kind)
            try fields.encode(path, forKey: .path)
            try fields.encode(parts, forKey: .parts)
        case .paired(let host):
            try fields.encode("paired", forKey: .kind)
            try fields.encode(host, forKey: .host)
        case .sendAttempt(let delivered, let reason):
            try fields.encode("sendAttempt", forKey: .kind)
            try fields.encode(delivered, forKey: .delivered)
            try fields.encodeIfPresent(reason, forKey: .reason)
        case .replayed(let state):
            try fields.encode("replayed", forKey: .kind)
            try fields.encode(state, forKey: .replayed)
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

    /// `-amux-door-port N`: listen here instead of on a port the kernel
    /// picks and a readiness file names.
    ///
    /// The readiness file lives in the app's own container, which a driver on
    /// the Mac reads back out of it. A UI test cannot: it runs on the device,
    /// in a container of its own, with no way into the app's. So a launch a UI
    /// test starts says which port to listen on, and the test connects to it
    /// over the loopback both processes share.
    public static let portArgument = "amux-door-port"

    /// `-amux-relay URL -amux-token BEARER -amux-user NAME`, and optionally
    /// `-amux-pair PAYLOAD`: connect the shared runtime as the launch starts,
    /// and trust the host that payload names before drawing anything.
    ///
    /// A driver that speaks through the door connects by asking. A UI test
    /// cannot ask — it launches the app and presses things — so a launch it
    /// starts says up front what to reach, and everything a tap then does
    /// happens against a real relay and a real machine.
    public static let relayArgument = "amux-relay"
    public static let tokenArgument = "amux-token"
    public static let userArgument = "amux-user"
    public static let pairArgument = "amux-pair"

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
