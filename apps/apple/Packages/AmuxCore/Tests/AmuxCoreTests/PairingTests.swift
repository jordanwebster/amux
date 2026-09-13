import Foundation
import XCTest

@testable import AmuxCore

@MainActor
final class PairingTests: XCTestCase {
    private let homelab = HostId(UUID(uuidString: "50000000-0000-0000-0000-000000000001")!)
    private let reading = Date(timeIntervalSince1970: 1_700_000_000)

    private func machine() -> HostEntry {
        HostEntry(
            id: homelab, name: "homelab", online: true, trustStatus: .untrustedButOnline,
            platform: "Linux")
    }

    /// A bundle with a connection behind it, remembering every command it was
    /// asked to send.
    private func bundle() -> (StoreBundle, Sent) {
        let sent = Sent()
        let stores = StoreBundle(account: AccountId("test"), clock: { [reading] in reading })
        stores.dispatch = { command in
            sent.commands.append(command)
            let op = OpId(UUID())
            sent.ops.append(op)
            return op
        }
        return (stores, sent)
    }

    private final class Sent {
        var commands: [BridgeCommand] = []
        var ops: [OpId] = []
    }

    private func offer(_ pending: String = "attempt-1") -> PendingPeer {
        PendingPeer(
            pending: pending, host: homelab, name: "homelab",
            fingerprint: String(repeating: "ab", count: 32),
            expiresAt: reading.addingTimeInterval(270))
    }

    // MARK: - The code

    /// Nothing leaves the phone until the sixth digit, and then exactly one
    /// thing does. Every extra send is one of the attempts the machine allows.
    func testACodeIsSentOnItsSixthDigitAndNotBefore() {
        let (stores, sent) = bundle()
        stores.pairing.open(machine: machine())

        for typed in ["4", "41", "419", "4197", "41972"] {
            stores.pair(digits: typed)
            XCTAssertEqual(sent.commands, [], "\(typed) was sent before the code was complete")
        }
        stores.pair(digits: "419723")

        XCTAssertEqual(sent.commands, [.beginPairPin(host: homelab, pin: "419723")])
        XCTAssertEqual(stores.pairing.phase, .checking)
        // A code that is with the machine is not a code being typed.
        XCTAssertFalse(stores.pairing.taking)
    }

    func testOnlyDigitsAndOnlySixOfThemAreACode() {
        let (stores, sent) = bundle()
        stores.pairing.open(machine: machine())

        stores.pair(digits: "4a1 9-7")
        XCTAssertEqual(stores.pairing.digits, "4197")
        XCTAssertEqual(sent.commands, [])

        stores.pair(digits: "41972345")
        XCTAssertEqual(stores.pairing.digits, "419723")
        XCTAssertEqual(sent.commands.count, 1)
    }

    /// The machine's account of itself, and no trust: the store moves to a
    /// decision rather than to a paired state.
    func testAnAuthenticatedCodeAsksRatherThanPairs() {
        let (stores, sent) = bundle()
        stores.pairing.open(machine: machine())
        stores.pair(digits: "419723")

        stores.apply([.opResult(OpResult(op: sent.ops[0], outcome: .pairingPending(offer())))])

        XCTAssertEqual(stores.pairing.phase, .confirming(offer()))
        // Confirming is a second command, and it is the only one that trusts.
        stores.confirmPairing(offer())
        XCTAssertEqual(sent.commands.last, .confirmPair(pending: "attempt-1"))
    }

    /// Mistyped, expired, already used, never issued, an answer to an attempt
    /// nobody holds, or an outright failure: one state and one empty entry.
    func testEveryFailureIsTheSameStateAndClearsTheDigits() {
        for outcome in [
            OpOutcome.pairingRefused,
            .pairingLost,
            try! AmuxJSON.decoder.decode(
                OpOutcome.self,
                from: Data(#"{"outcome":"error","error":{"error":"Unavailable"}}"#.utf8)),
        ] {
            let (stores, sent) = bundle()
            stores.pairing.open(machine: machine())
            stores.pair(digits: "419723")

            stores.apply([.opResult(OpResult(op: sent.ops[0], outcome: outcome))])

            XCTAssertEqual(stores.pairing.phase, .refused, "\(outcome)")
            XCTAssertEqual(stores.pairing.digits, "", "\(outcome) left the digits on screen")
            // The next code can be typed straight over the refusal.
            XCTAssertTrue(stores.pairing.taking)
        }
    }

    func testTypingOverARefusalTakesItOffTheScreen() {
        let (stores, sent) = bundle()
        stores.pairing.open(machine: machine())
        stores.pair(digits: "419723")
        stores.apply([.opResult(OpResult(op: sent.ops[0], outcome: .pairingRefused))])

        stores.pair(digits: "5")

        XCTAssertEqual(stores.pairing.phase, .entering)
        XCTAssertEqual(stores.pairing.digits, "5")
    }

    /// A result belonging to something else does not answer this attempt.
    func testAnotherOperationsResultIsNotThisAttemptsAnswer() {
        let (stores, sent) = bundle()
        stores.pairing.open(machine: machine())
        stores.pair(digits: "419723")

        stores.apply([.opResult(OpResult(op: OpId(UUID()), outcome: .pairingRefused))])
        XCTAssertEqual(stores.pairing.phase, .checking)

        stores.apply([.opResult(OpResult(op: sent.ops[0], outcome: .pairingPending(offer())))])
        XCTAssertEqual(stores.pairing.phase, .confirming(offer()))
    }

    /// With no machine there is nothing to authenticate against, so a complete
    /// code goes nowhere rather than to whichever machine happens to be first.
    func testACodeWithNoMachineIsNotSentAnywhere() {
        let (stores, sent) = bundle()
        stores.pairing.open()

        stores.pair(digits: "419723")

        XCTAssertEqual(sent.commands, [])
        XCTAssertEqual(stores.pairing.digits, "419723")
    }

    // MARK: - The link

    func testALinkIsAuthenticatedAndTrustsNobody() {
        let (stores, sent) = bundle()

        XCTAssertTrue(stores.pair(link: "payload-as-it-arrived"))

        XCTAssertEqual(sent.commands, [.beginPairLink(payload: "payload-as-it-arrived")])
        XCTAssertEqual(stores.pairing.phase, .checking)

        stores.apply([.opResult(OpResult(op: sent.ops[0], outcome: .pairingPending(offer())))])
        XCTAssertEqual(stores.pairing.phase, .confirming(offer()))
    }

    /// A link that arrives before this phone has an account cannot be asked
    /// about, and saying so is what lets the page ask again once it can.
    func testALinkWithNoConnectionBehindItIsNotSpent() {
        let stores = StoreBundle(account: AccountId("signed-out"), clock: { [reading] in reading })

        XCTAssertFalse(stores.pair(link: "payload-as-it-arrived"))
    }

    func testTurningAMachineAwayTellsItSo() {
        let (stores, sent) = bundle()

        stores.abandonPairing(offer())

        XCTAssertEqual(sent.commands, [.abandonPair(pending: "attempt-1")])
        stores.apply([.opResult(OpResult(op: sent.ops[0], outcome: .pairingAbandoned))])
        XCTAssertEqual(stores.pairing.phase, .entering)
    }

    func testConfirmingReportsTheMachineItTrusted() {
        let (stores, sent) = bundle()
        stores.confirmPairing(offer())

        stores.apply([
            .opResult(OpResult(op: sent.ops[0], outcome: .paired(host: homelab, name: "homelab")))
        ])

        XCTAssertEqual(stores.pairing.phase, .trusted("homelab"))
    }

    // MARK: - What the bridge is told

    /// The wire spelling, pinned. A rename on either side of the boundary
    /// breaks here rather than becoming a pairing step the runtime ignores.
    func testPairingCommandsAreSpeltTheWayTheRuntimeReadsThem() throws {
        let encoder = JSONEncoder()
        encoder.outputFormatting = .sortedKeys
        let spelt = { (command: BridgeCommand) in
            String(decoding: try! encoder.encode(command), as: UTF8.self)
        }
        XCTAssertEqual(
            spelt(.beginPairPin(host: homelab, pin: "419723")),
            """
            {"command":"begin_pair_pin","host":"50000000-0000-0000-0000-000000000001",\
            "pin":"419723"}
            """)
        XCTAssertEqual(
            spelt(.beginPairLink(payload: "abc")),
            #"{"command":"begin_pair_link","payload":"abc"}"#)
        XCTAssertEqual(
            spelt(.confirmPair(pending: "attempt-1")),
            #"{"command":"confirm","pending":"attempt-1"}"#)
        XCTAssertEqual(
            spelt(.abandonPair(pending: "attempt-1")),
            #"{"command":"abandon","pending":"attempt-1"}"#)
    }

    /// A pending peer as the runtime writes one, read back whole. The
    /// capability that commits trust is not in it: what crosses the boundary
    /// is a handle and what a person needs to look at.
    func testAPendingPeerIsReadAsTheRuntimeWroteIt() throws {
        let json = """
            {"outcome":"pairing_pending","pending":"attempt-1",
             "host":"50000000-0000-0000-0000-000000000001","name":"homelab",
             "fingerprint":"4bb06f8e4e3a7715d201d573d0aa423762e55dabd61a2c02278fa56cc6d294e0",
             "expires_at":"2023-11-14T22:18:50Z"}
            """
        let outcome = try AmuxJSON.decoder.decode(OpOutcome.self, from: Data(json.utf8))

        guard case .pairingPending(let peer) = outcome else {
            return XCTFail("expected a pending peer, got \(outcome)")
        }
        XCTAssertEqual(peer.pending, "attempt-1")
        XCTAssertEqual(peer.host, homelab)
        XCTAssertEqual(peer.name, "homelab")
        XCTAssertEqual(
            peer.fingerprint,
            "4bb06f8e4e3a7715d201d573d0aa423762e55dabd61a2c02278fa56cc6d294e0")
        XCTAssertEqual(peer.expiresAt, Date(timeIntervalSince1970: 1_700_000_330))
    }

    /// Every pairing answer the runtime can give, in its own spelling.
    func testEveryPairingOutcomeIsRead() throws {
        let read = { (json: String) in
            try AmuxJSON.decoder.decode(OpOutcome.self, from: Data(json.utf8))
        }
        XCTAssertEqual(
            try read(#"{"outcome":"paired","host":"50000000-0000-0000-0000-000000000001","name":"homelab"}"#),
            .paired(host: homelab, name: "homelab"))
        XCTAssertEqual(try read(#"{"outcome":"pairing_abandoned"}"#), .pairingAbandoned)
        XCTAssertEqual(try read(#"{"outcome":"pairing_refused"}"#), .pairingRefused)
        XCTAssertEqual(try read(#"{"outcome":"pairing_lost"}"#), .pairingLost)
    }

    /// Machines nobody has paired with are not the fleet, and the pairing
    /// screens are the only thing that reads them.
    func testDiscoveredMachinesAreKeptApartFromTheFleet() {
        let stores = StoreBundle(account: AccountId("test"), clock: { [reading] in reading })

        stores.apply([
            .fleet(Fleet(epoch: 1, agents: [], hosts: [], reconciled: true)),
            .discovered([machine()]),
        ])

        XCTAssertEqual(stores.hosts.hosts, [])
        XCTAssertEqual(stores.hosts.discovered.map(\.name), ["homelab"])
        XCTAssertEqual(stores.hosts.known(homelab)?.name, "homelab")
        XCTAssertNil(stores.hosts.host(homelab))
    }
}
