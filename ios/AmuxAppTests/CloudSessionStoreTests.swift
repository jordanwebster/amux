import Foundation
import XCTest
import AmuxCore

final class CloudSessionStoreTests: XCTestCase {
    func testKeychainRoundTripRotationIsolationAndRemoval() throws {
        let service = "sh.amux.tests.refresh.\(UUID().uuidString)"
        let saved = KeychainCloudSessions(service: service)
        let account = AccountId("first")
        let other = AccountId("second")
        defer {
            try? saved.write(nil, for: account)
            try? saved.write(nil, for: other)
        }
        XCTAssertNil(saved.read(account))
        try saved.write("original-token", for: account)
        try saved.write("other-token", for: other)
        let reopened = KeychainCloudSessions(service: service)
        XCTAssertEqual(reopened.read(account), "original-token")
        try reopened.write("rotated-token", for: account)
        XCTAssertEqual(saved.read(account), "rotated-token")
        XCTAssertEqual(saved.read(other), "other-token")
        try reopened.write(nil, for: account)
        XCTAssertNil(saved.read(account))
        try reopened.write(nil, for: account)
        XCTAssertEqual(saved.read(other), "other-token")
    }
}
