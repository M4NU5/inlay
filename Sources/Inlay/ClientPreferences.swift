import AppKit
import Combine
import Foundation
import Security
import InlayCore

/// Only this Mac's identity and connections live here. Generation state and
/// processing preferences belong to the server.
@MainActor
final class ClientPreferencesStore: ObservableObject {
    static let defaultEndpoint = "http://127.0.0.1:8391"
    static let maximumEndpoints = 5
    private struct Settings: Codable {
        var endpoints: [String]?
        /// Read only from single-server files made before failover. Still written, as the
        /// first server, so an older build can read the file.
        var endpoint: String?
        var deviceID: String
        var deviceName: String
    }

    /// Servers in priority order. The controller picks the active one from their health.
    @Published private(set) var endpoints: [String]
    @Published private(set) var deviceID: String
    @Published private(set) var deviceName: String
    /// Keyed by endpoint address. Each credential is only ever sent to its own server.
    @Published private(set) var tokens: [String: String]
    @Published private(set) var errorMessage: String?
    private let url: URL
    private let credentialAccount: String
    /// The list client.json keeps. An INLAY_SERVER_URL run edits `endpoints` only.
    private var savedEndpoints: [String]
    private let isOverridden: Bool

    init(root: URL, environment: [String: String] = ProcessInfo.processInfo.environment,
         readCredential: ((String) -> String)? = nil) {
        url = root.appendingPathComponent("client.json")
        credentialAccount = root.standardizedFileURL.path
        let saved = (try? Data(contentsOf: url)).flatMap { try? JSONDecoder().decode(Settings.self, from: $0) }
        let savedEndpoints = Self.normalized(saved?.endpoints ?? saved?.endpoint.map { [$0] } ?? [])
        let override = environment["INLAY_SERVER_URL"].map { Self.normalized([$0]) }
        let saveable = savedEndpoints.isEmpty ? [Self.defaultEndpoint] : savedEndpoints
        let resolved = override ?? saveable
        endpoints = resolved
        self.savedEndpoints = saveable
        isOverridden = override != nil
        deviceID = saved?.deviceID ?? UUID().uuidString.lowercased()
        deviceName = saved?.deviceName ?? Host.current().localizedName ?? "My Mac"
        tokens = [:]
        // persist() writes the saved list, never an INLAY_SERVER_URL override for this run.
        if saved == nil, saveable.allSatisfy({ (try? ServerEndpoint($0)) != nil }) { persist() }
        else if saved?.endpoints == nil, !savedEndpoints.isEmpty { persist() }
        for endpoint in resolved {
            do {
                _ = try ServerEndpoint(endpoint)
                let account = credentialAccount + "|" + endpoint
                tokens[endpoint] = readCredential?(account) ?? Self.readToken(account: account)
            } catch {
                errorMessage = error.localizedDescription
            }
        }
    }

    func token(for endpoint: String) -> String { tokens[endpoint] ?? "" }

    /// Normalizes valid addresses so they match their Keychain accounts. Invalid
    /// entries stay as written so the connection form can show and fix them.
    private static func normalized(_ values: [String]) -> [String] {
        var result: [String] = []
        for value in values {
            let address = (try? ServerEndpoint(value).address) ?? value
            if !result.contains(address) { result.append(address) }
        }
        return Array(result.prefix(maximumEndpoints))
    }

    @discardableResult
    func save(servers: [(endpoint: String, token: String)], deviceName: String) -> Bool {
        guard (1...Self.maximumEndpoints).contains(servers.count) else {
            errorMessage = "Add between 1 and \(Self.maximumEndpoints) servers."
            return false
        }
        var validated: [(endpoint: String, token: String)] = []
        for (index, server) in servers.enumerated() {
            let label = servers.count > 1 ? "Server \(index + 1): " : ""
            let address: String
            do { address = try ServerEndpoint(server.endpoint).address }
            catch {
                errorMessage = label + error.localizedDescription
                return false
            }
            guard !validated.contains(where: { $0.endpoint == address }) else {
                errorMessage = label + "This server is already in the list."
                return false
            }
            let trimmedToken = server.token.trimmingCharacters(in: .whitespacesAndNewlines)
            guard trimmedToken.utf8.count <= 4_096, !trimmedToken.contains(where: \.isWhitespace) else {
                errorMessage = label + "Use a server token without whitespace, up to 4 KB."
                return false
            }
            validated.append((address, trimmedToken))
        }
        let name = deviceName.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !name.isEmpty, name.count <= 120 else {
            errorMessage = "Use a device name between 1 and 120 characters."
            return false
        }
        // Scope credentials to their exact endpoint so a failed settings write
        // or environment override cannot send one server's key to another.
        for server in validated {
            guard Self.writeToken(server.token, account: credentialAccount + "|" + server.endpoint) else {
                errorMessage = "The server credential could not be saved in Keychain."
                return false
            }
        }
        let previous = (self.endpoints, self.deviceName, self.tokens, self.savedEndpoints)
        self.endpoints = validated.map(\.endpoint)
        self.deviceName = name
        self.tokens = Dictionary(uniqueKeysWithValues: validated.map { ($0.endpoint, $0.token) })
        if !isOverridden { savedEndpoints = self.endpoints }
        guard persist() else {
            (self.endpoints, self.deviceName, self.tokens, self.savedEndpoints) = previous
            return false
        }
        // Only after the list stops naming a server, so a failed write keeps its credential.
        // Servers client.json still lists keep theirs during an override run.
        for removed in previous.0 where !self.endpoints.contains(removed) && !savedEndpoints.contains(removed) {
            if let address = try? ServerEndpoint(removed).address {
                Self.writeToken("", account: credentialAccount + "|" + address)
            }
        }
        return true
    }

    @discardableResult
    func replaceToken(_ newToken: String, for endpoint: String) -> Bool {
        guard endpoints.contains(endpoint) else { return false }
        let trimmed = newToken.trimmingCharacters(in: .whitespacesAndNewlines)
        guard Self.writeToken(trimmed, account: credentialAccount + "|" + endpoint) else {
            errorMessage = "The server credential could not be saved in Keychain."
            return false
        }
        tokens[endpoint] = trimmed
        return true
    }

    @discardableResult
    private func persist() -> Bool {
        do {
            try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true,
                                                    attributes: [.posixPermissions: 0o700])
            let encoder = JSONEncoder()
            encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
            // `endpoint` lets an older build roll back without losing this Mac's identity.
            let data = try encoder.encode(Settings(endpoints: savedEndpoints, endpoint: savedEndpoints.first,
                                                   deviceID: deviceID, deviceName: deviceName))
            try data.write(to: url, options: .atomic)
            try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: url.path)
            errorMessage = nil
            return true
        } catch {
            errorMessage = "Could not save device preferences: \(error.localizedDescription)"
            return false
        }
    }

    private static func keychainQuery(account: String) -> [String: Any] {
        [kSecClass as String: kSecClassGenericPassword,
         kSecAttrService as String: InlayBuild.current.credentialService,
         kSecAttrAccount as String: account]
    }

    private static func readToken(account: String) -> String {
        var query = keychainQuery(account: account)
        query[kSecReturnData as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: CFTypeRef?
        guard SecItemCopyMatching(query as CFDictionary, &result) == errSecSuccess,
              let data = result as? Data else { return "" }
        return String(data: data, encoding: .utf8) ?? ""
    }

    @discardableResult
    private static func writeToken(_ token: String, account: String) -> Bool {
        let query = keychainQuery(account: account)
        if token.isEmpty {
            let status = SecItemDelete(query as CFDictionary)
            return status == errSecSuccess || status == errSecItemNotFound
        }
        let attributes: [String: Any] = [kSecValueData as String: Data(token.utf8)]
        let status = SecItemUpdate(query as CFDictionary, attributes as CFDictionary)
        if status == errSecSuccess { return true }
        guard status == errSecItemNotFound else { return false }
        var entry = query
        entry[kSecValueData as String] = Data(token.utf8)
        entry[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
        return SecItemAdd(entry as CFDictionary, nil) == errSecSuccess
    }
}
