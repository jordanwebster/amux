import Foundation
import XCTest

final class ProductionStartupTests: JourneyCase {
    @MainActor
    func testSignInStartsTheAppAndRelaunchDrawsTheRememberedAccount() throws {
        let runner = try Runner()
        let relay = try XCTUnwrap(URLComponents(string: runner.relay))
        let environment = ProcessInfo.processInfo.environment
        let machine = try XCTUnwrap(environment["AMUX_HOST_ID"])
        let script: [String: Any] = [
            "account": runner.user, "email": "ada@example.com", "displayName": "Ada",
            "entitlement": "active", "source": "granted", "token": runner.token,
            "relayHost": try XCTUnwrap(relay.host), "relayPort": try XCTUnwrap(relay.port),
        ]
        let app = XCUIApplication()
        func arguments(_ state: [String: Any]) throws -> [String] {
            let json = try JSONSerialization.data(withJSONObject: state, options: [.sortedKeys])
            return ["-amux-door-port", runner.doorPort, "-amux-scripted-cloud",
                    "-amux-cloud-script", String(decoding: json, as: UTF8.self)]
        }
        app.launchArguments = try arguments(script)
        record["launchArguments"] = app.launchArguments.filter { $0.hasPrefix("-amux-") }
        defer { try? write("production-startup.json") }
        app.launch()
        waitFor(app, "home.empty.action", "the unsigned home did not offer sign-in")
        press(app, "home.empty.action")
        waitFor(app, "sign-in", "sign-in did not open")
        press(app, "sign-in.continue")
        waitFor(app, "sign-in.signed-in", "the scripted cloud did not sign in")
        press(app, "sign-in.continue")
        _ = try waitForValue(runner, "home", "ready")

        // Trust is established after sign-in; no runtime or credential is supplied
        // through the door. Pairing is the same protocol the keypad speaks.
        let control = try Lines(address: runner.control)
        let reply = try control.ask(["StartPinPairing": ["daemon": runner.host, "ttl_secs": 600]])
        let pin = try XCTUnwrap((reply["Ack"] as? [String: Any])?["pin"] as? String)
        try door(runner, .init(kind: "pairByCode", host: machine, pin: pin))
        waitFor(app, "home.row.\(runner.agent)", "the app's startup did not receive the host's agent")
        photograph(app, "startup-home")
        pressTab(app, "Hosts")
        waitFor(app, "hosts", "the Hosts tab did not open")
        XCTAssertTrue(waitUntil { ((try? self.declared(runner)) ?? []).contains { $0.label.contains(runner.host) } })
        photograph(app, "startup-hosts")
        record["connected"] = try door(runner, .init(kind: "bridge"))
        let calls = try door(runner, .init(kind: "calls"))
        record["calls"] = calls
        let credentials = (calls["cloud"] as? [String] ?? []).filter { $0.hasPrefix("connectToken ") }
        XCTAssertLessThanOrEqual(credentials.count, 4,
                                 "redrawing the root must not construct another runtime coordinator")
        app.terminate()

        // Hold the service's answer, so the only possible source of these rows is
        // the account and fleet the first launch saved. No fixture seeds either.
        app.launchArguments = try arguments(script.merging(["latencyMillis": 60_000]) { _, new in new })
        app.launch()
        waitFor(app, "home.row.\(runner.agent)", "relaunch did not draw the cached agent")
        record["restoredAccounts"] = try door(runner, .init(kind: "accounts"))
        record["beforeReconciliation"] = try door(runner, .init(kind: "bridge"))
        record["rememberedRows"] = try declared(runner).filter { $0.identifier.hasPrefix("home.row.") }.map {
            ["identifier": $0.identifier, "value": $0.value, "label": $0.label]
        }
        photograph(app, "startup-remembered")
        let restored = try door(runner, .init(kind: "accounts"))
        let state = try XCTUnwrap(restored["known"] as? [String: Any])
        XCTAssertEqual(state["selected"] as? String, runner.user)
        let before = try door(runner, .init(kind: "bridge"))
        XCTAssertEqual((before["bridge"] as? [String: Any])?["reconciled"] as? Bool, false)
        XCTAssertEqual((before["bridge"] as? [String: Any])?["started"] as? Bool, false)

        try door(runner, .init(kind: "cloud", cloud: script))
        XCUIDevice.shared.press(.home)
        app.activate()
        try door(runner, .init(kind: "awaitReconciled", seconds: 60))
        waitFor(app, "home.row.\(runner.agent)", "the restored profile did not reconcile with its host")
        record["reconciled"] = try door(runner, .init(kind: "bridge"))
    }
}
