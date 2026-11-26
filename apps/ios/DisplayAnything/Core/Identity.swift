import Foundation
import Security

/// This phone's certificate and key. Kept in the Keychain, on this device
/// only (never synced to iCloud), so it survives relaunches.
enum PhoneIdentity {
    private static let service = "com.displayanything.viewer.identity"

    static func loadOrCreate(deviceName: String) throws -> IdentityBytes {
        if let existing = try load() { return existing }
        let fresh = try generateIdentity(deviceName: deviceName)
        try save(fresh)
        return fresh
    }

    private static func load() throws -> IdentityBytes? {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecReturnData as String: true,
        ]
        var item: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &item)
        if status == errSecItemNotFound { return nil }
        guard status == errSecSuccess, let data = item as? Data else {
            throw KeychainError(status: status)
        }
        return try decode(data)
    }

    private static func save(_ id: IdentityBytes) throws {
        let attributes: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccessible as String: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly,
            kSecValueData as String: try encode(id),
        ]
        let status = SecItemAdd(attributes as CFDictionary, nil)
        guard status == errSecSuccess else { throw KeychainError(status: status) }
    }

    // Two length-prefixed blobs: cert, then key.
    private static func encode(_ id: IdentityBytes) throws -> Data {
        var out = Data()
        for blob in [id.certDer, id.keyDer] {
            var len = UInt32(blob.count).bigEndian
            out.append(Data(bytes: &len, count: 4))
            out.append(blob)
        }
        return out
    }

    private static func decode(_ data: Data) throws -> IdentityBytes {
        var offset = 0
        func next() throws -> Data {
            guard offset + 4 <= data.count else { throw KeychainError(status: errSecDecode) }
            let len = data[offset..<offset + 4].withUnsafeBytes { $0.loadUnaligned(as: UInt32.self) }
            offset += 4
            let end = offset + Int(UInt32(bigEndian: len))
            guard end <= data.count else { throw KeychainError(status: errSecDecode) }
            defer { offset = end }
            return Data(data[offset..<end])
        }
        return IdentityBytes(certDer: try next(), keyDer: try next())
    }
}

struct KeychainError: LocalizedError {
    let status: OSStatus
    var errorDescription: String? {
        "Could not read this phone's saved identity (Keychain status \(status))."
    }
}
