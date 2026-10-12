import Foundation
import Combine
import CryptoKit
import InlayCore

/// Where models live and how a packaged server is found. Single-sources the
/// Application Support layout shared by the download manager, the local
/// server manager, and scripts/install-service.sh.
enum LocalServerPaths {
    static var supportDirectory: URL {
        InlayBuild.current.dataDirectory.deletingLastPathComponent()
    }
    static var dataDirectory: URL { supportDirectory.appendingPathComponent("ServerData", isDirectory: true) }
    static var modelsDirectory: URL { supportDirectory.appendingPathComponent("Models", isDirectory: true) }
    static var logsDirectory: URL { supportDirectory.appendingPathComponent("Logs", isDirectory: true) }
    static var tokenFile: URL { dataDirectory.appendingPathComponent("admin-token") }
    static var markerFile: URL { dataDirectory.appendingPathComponent("local-server.json") }
    static var plistURL: URL {
        FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Library/LaunchAgents/dev.davis.inlay.server-agent.plist")
    }
    static let label = "dev.davis.inlay.server-agent"
    static var bundledServerDirectory: URL? {
        Bundle.main.bundleURL.appendingPathComponent("Contents/Helpers/Server", isDirectory: true)
            .nilIfMissing
    }
    static var installedMarkerExists: Bool { FileManager.default.fileExists(atPath: markerFile.path) }
}

private extension URL {
    var nilIfMissing: URL? { FileManager.default.fileExists(atPath: path) ? self : nil }
}

/// Immutable record of an app-managed server install. Uninstall trusts only
/// this record; anything else on disk belongs to the user or a source build.
struct LocalServerMarker: Codable {
    var label: String
    var plistPath: String
    var serverPath: String
    var dataDirectory: String
    var installedAt: Date
}

/// Downloads and verifies the pinned model weights the same way
/// scripts/download-model.sh and scripts/download-text-model.sh do: size +
/// SHA-256 per file, `.download` siblings renamed into place, re-runnable
/// as repair.
@MainActor
final class ModelDownloadManager: ObservableObject {
    @Published private(set) var speechStatus: ModelStatus = .missing
    @Published private(set) var textStatus: ModelStatus = .missing
    @Published private(set) var isDownloading = false
    @Published var errorMessage: String?

    private var downloadTask: Task<Void, Never>?
    private let modelsDirectory: URL
    private let session: URLSession

    init(modelsDirectory: URL = LocalServerPaths.modelsDirectory, session: URLSession? = nil) {
        self.modelsDirectory = modelsDirectory
        if let session { self.session = session } else {
            let configuration = URLSessionConfiguration.ephemeral
            configuration.timeoutIntervalForRequest = 120
            configuration.timeoutIntervalForResource = 7_200
            configuration.waitsForConnectivity = true
            self.session = URLSession(configuration: configuration)
        }
        refreshStatus()
    }

    private var speechModelURL: URL {
        modelsDirectory.appendingPathComponent(SpeechModel.parakeet.filename)
    }
    private var textModelDirectory: URL {
        modelsDirectory.appendingPathComponent(TextModel.qwen.filename, isDirectory: true)
    }

    func refreshStatus() {
        speechStatus = ModelIntegrity.verify(speechModelURL).modelStatus
        textStatus = TextModel.qwen.verify(textModelDirectory).modelStatus
    }

    func downloadSpeechModel() {
        let pin = SpeechModel.parakeet
        download([(pin.downloadURL, speechModelURL, pin.byteCount, pin.sha256)],
                 directory: modelsDirectory) { [weak self] in self?.refreshStatus() }
    }

    func downloadTextModel() {
        let files = TextModel.qwen.files.map { file in
            (file.downloadURL, textModelDirectory.appendingPathComponent(file.filename), file.byteCount, file.sha256)
        }
        download(files, directory: textModelDirectory) { [weak self] in
            // Mirror the shell script's extra-file rejection at the boundary.
            guard let self else { return }
            let expected = Set(TextModel.qwen.files.map(\.filename))
            let present = Set((try? FileManager.default.contentsOfDirectory(atPath: textModelDirectory.path)) ?? [])
            let extras = present.subtracting(expected).sorted()
            if !extras.isEmpty {
                errorMessage = "Remove unexpected files from the model folder: \(extras.joined(separator: ", "))."
            }
            refreshStatus()
        }
    }

    func cancelDownloads() { downloadTask?.cancel() }

    private func download(_ files: [(URL, URL, Int64, String)], directory: URL, onFinish: @escaping () -> Void) {
        guard downloadTask == nil else { return }
        errorMessage = nil
        isDownloading = true
        downloadTask = Task { [weak self] in
            guard let self else { return }
            defer {
                downloadTask = nil
                isDownloading = false
                onFinish()
            }
            do {
                try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true,
                                                        attributes: [.posixPermissions: 0o755])
                for (source, target, byteCount, sha256) in files {
                    try Task.checkCancellation()
                    if Self.verifyFile(at: target, byteCount: byteCount, sha256: sha256) { continue }
                    let partial = target.appendingPathExtension("download")
                    let (downloaded, response) = try await session.download(from: source)
                    guard let http = response as? HTTPURLResponse, (200...299).contains(http.statusCode) else {
                        throw CocoaError(.fileReadUnknown)
                    }
                    try? FileManager.default.removeItem(at: partial)
                    try FileManager.default.moveItem(at: downloaded, to: partial)
                    guard Self.verifyFile(at: partial, byteCount: byteCount, sha256: sha256) else {
                        try? FileManager.default.removeItem(at: partial)
                        errorMessage = "A download failed its integrity check; run the download again."
                        return
                    }
                    try? FileManager.default.removeItem(at: target)
                    try FileManager.default.moveItem(at: partial, to: target)
                }
            } catch is CancellationError {
                // Intentional: leave verified files in place, no message.
            } catch {
                errorMessage = "The model download could not finish. Check the connection and try again."
            }
        }
    }

    static func verifyFile(at url: URL, byteCount: Int64, sha256: String) -> Bool {
        guard let attributes = try? FileManager.default.attributesOfItem(atPath: url.path),
              let size = attributes[.size] as? NSNumber, size.int64Value == byteCount,
              let handle = try? FileHandle(forReadingFrom: url) else { return false }
        defer { try? handle.close() }
        var hasher = SHA256()
        while let chunk = try? handle.read(upToCount: 1_048_576), !chunk.isEmpty { hasher.update(data: chunk) }
        return hasher.finalize().map { String(format: "%02x", $0) }.joined() == sha256
    }
}

extension Result where Success == Void, Failure == ModelIntegrityError {
    fileprivate var modelStatus: ModelStatus {
        switch self {
        case .success: return .installed
        case .failure: return .missing
        }
    }
}
