import Foundation
import InlayAPI

/// One server's latest health check. Readiness alone decides failover: a
/// server that answers but cannot take dictation counts as down.
struct ServerProbe: Equatable, Sendable {
    let health: ServerHealth?
    /// Why this server cannot take dictation; nil when it can.
    let problem: String?
    let needsEnrollment: Bool
    let checkedAt: Date

    init(health: ServerHealth, checkedAt: Date = Date()) {
        self.health = health
        problem = health.apiVersion != InlayAPI.version ? "Server API version is incompatible"
            : (health.ready ? nil : (health.message ?? "Server models are not ready"))
        needsEnrollment = false
        self.checkedAt = checkedAt
    }

    init(error: Error, checkedAt: Date = Date()) {
        health = nil
        problem = Self.message(for: error)
        if case .rejected(403, _, "enrollment_required")? = error as? ServerClientError {
            needsEnrollment = true
        } else { needsEnrollment = false }
        self.checkedAt = checkedAt
    }

    var isReady: Bool { problem == nil }
    var isReachable: Bool { health != nil }

    static func message(for error: Error) -> String {
        if let urlError = error as? URLError {
            switch urlError.code {
            case .cannotFindHost, .dnsLookupFailed:
                return "Server not found at this address · Recording unavailable"
            case .timedOut:
                return "Server connection timed out · Recording unavailable"
            case .secureConnectionFailed, .serverCertificateHasBadDate, .serverCertificateUntrusted,
                 .serverCertificateHasUnknownRoot, .serverCertificateNotYetValid, .clientCertificateRejected:
                return "Secure connection to the server failed · Recording unavailable"
            default:
                return "Server offline · Recording unavailable"
            }
        }
        return error.localizedDescription
    }

    /// Errors that mean this server cannot admit a take right now, so another
    /// ready server may. A rejection of the request itself is not one.
    static func isUnavailable(_ error: Error) -> Bool {
        if let urlError = error as? URLError { return urlError.code != .cancelled }
        if case .rejected(503, _, _)? = error as? ServerClientError { return true }
        return false
    }
}

enum ServerFailover {
    /// The first server in priority order that can take dictation now.
    static func preferred(_ endpoints: [String], probes: [String: ServerProbe]) -> String? {
        endpoints.first { probes[$0]?.isReady == true }
    }

    /// Host and port only, for status text.
    static func displayName(_ endpoint: String) -> String {
        guard let url = try? ServerEndpoint(endpoint).url, let host = url.host else { return endpoint }
        return url.port.map { "\(host):\($0)" } ?? host
    }
}
