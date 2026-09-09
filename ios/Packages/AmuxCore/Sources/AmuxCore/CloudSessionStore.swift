import Foundation
import Security

/// Only refresh tokens survive launch. Account names and grants live in the registry.
public protocol CloudSessionStore: Sendable {
    func read(_ account: AccountId) -> String?
    func write(_ token: String?, for account: AccountId) throws
}

/// Secrets stay on this phone and are available after its first unlock, including
/// when an in-flight refresh finishes as the app is being put away.
public struct KeychainCloudSessions: CloudSessionStore {
    private let service: String

    public init(service: String = "sh.amux.app.refresh") {
        self.service = service
    }

    private func query(_ account: AccountId) -> [String: Any] {
        [kSecClass as String: kSecClassGenericPassword,
         kSecAttrService as String: service, kSecAttrAccount as String: account.value]
    }

    public func read(_ account: AccountId) -> String? {
        var request = query(account)
        request[kSecReturnData as String] = true
        request[kSecMatchLimit as String] = kSecMatchLimitOne
        var found: CFTypeRef?
        guard SecItemCopyMatching(request as CFDictionary, &found) == errSecSuccess,
              let data = found as? Data else { return nil }
        return String(data: data, encoding: .utf8)
    }

    public func write(_ token: String?, for account: AccountId) throws {
        let request = query(account)
        guard let token else {
            let status = SecItemDelete(request as CFDictionary)
            guard status == errSecSuccess || status == errSecItemNotFound else {
                throw CloudError.keychain("This phone could not forget the sign-in", status: status)
            }
            return
        }
        let values: [String: Any] = [
            kSecValueData as String: Data(token.utf8),
            kSecAttrAccessible as String: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly,
        ]
        var status = SecItemUpdate(request as CFDictionary, values as CFDictionary)
        if status == errSecItemNotFound {
            status = SecItemAdd(request.merging(values) { _, value in value } as CFDictionary, nil)
        }
        guard status == errSecSuccess else {
            throw CloudError.keychain(
                "This phone could not remember the sign-in. Please try again.", status: status)
        }
    }
}
