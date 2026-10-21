import AppKit
import Combine
import InlayAPI
import InlayCore
import Network
import ServiceManagement

private enum DictationDestination: Equatable {
    case test
    case field(InsertionTarget)
}

/// The desktop never owns a durable generation or a model process. A take has
/// one accepted server generation, one bounded upload, and at most one delivery.
enum InsertionMode {
    case insertion
    case clipboardOnly
    case unavailable
}

@MainActor
final class InlayController: ObservableObject {
    @Published var activity: DictationActivity = .idle {
        // A server switch waits for capture to end, then applies at once.
        didSet { if oldValue.isCapturing, !activity.isCapturing { applyProbes() } }
    }
    let recordingFeedback = RecordingFeedback()
    @Published var recordingListHint: String?
    @Published private(set) var recordingInputName: String?
    @Published var lastTranscript = ""
    @Published var lastTranscriptionSeconds: Double?
    @Published var lastAudioSeconds: Double?
    @Published var lastDelivery = ""
    @Published private(set) var lastDeliveryStatus: DictationDeliveryStatus = .none
    @Published private(set) var attentionPresented = false
    @Published var errorMessage: String?
    @Published var permissions: PermissionSnapshot
    @Published var isHotkeyActive = false
    @Published private(set) var isCheckingShortcut = false
    @Published private(set) var shortcutCheckText = ""
    @Published var shortcut: HoldKey = .rightOption {
        didSet {
            if shortcut != oldValue { stopShortcutCheck() }
            if !applyingConfiguration { configuration.update { $0.holdKey = shortcut.rawValue } }
            hotkey.key = shortcut
        }
    }
    @Published var customTriggerKeyCode: Int? {
        didSet {
            if customTriggerKeyCode != oldValue { stopShortcutCheck() }
            if !applyingConfiguration {
                let code = customTriggerKeyCode
                configuration.update { $0.customTriggerKeyCode = code }
            }
            hotkey.customKeyCode = customTriggerKeyCode.map { CGKeyCode($0) }
        }
    }
    @Published var activationMode: HotkeyActivationMode = .hold {
        didSet {
            guard activationMode != oldValue else { return }
            if !applyingConfiguration { configuration.update { $0.activationMode = activationMode.rawValue } }
            hotkey.mode = activationMode
        }
    }
    @Published var launchAtLogin = false {
        didSet {
            guard hasInitialized, !applyingConfiguration, !updatingLogin, launchAtLogin != oldValue else { return }
            configuration.update { $0.launchAtLogin = launchAtLogin }
            updateLoginItem()
        }
    }
    @Published private(set) var loginItemError: String?
    @Published var muteOutputWhileRecording = false {
        didSet {
            if !applyingConfiguration { configuration.update { $0.muteOutputWhileRecording = muteOutputWhileRecording } }
        }
    }
    @Published var dictationSoundsEnabled = true {
        didSet {
            if !applyingConfiguration { configuration.update { $0.dictationSoundsEnabled = dictationSoundsEnabled } }
        }
    }
    @Published var preserveClipboard = false {
        didSet {
            if !applyingConfiguration { configuration.update { $0.preserveClipboard = preserveClipboard } }
        }
    }
    @Published var statusMessage = "Connecting to server…"
    @Published private(set) var hudProgressText: String?
    @Published private(set) var serverHealth: ServerHealth?
    /// The server that history, shared settings and new takes use. It follows
    /// health checks but never changes while the microphone is capturing.
    @Published private(set) var activeEndpoint = ClientPreferencesStore.defaultEndpoint
    /// Each listed server's latest health check, keyed by endpoint address.
    @Published private(set) var serverProbes: [String: ServerProbe] = [:]
    @Published private(set) var serverStatusMessage = "Connecting…"
    @Published private(set) var isCheckingServer = false
    @Published private(set) var isSavingPreferences = false
    @Published private(set) var sharedPreferences: PreferencesSnapshot?
    @Published private(set) var storageUsage: StorageUsage?
    let modelDownloads = ModelDownloadManager()
    let localServer = LocalServerManager()
    @Published private(set) var generations: [GenerationSummary] = []
    @Published private(set) var generationDetail: GenerationRecord?
    @Published private(set) var generationDetailFailedID: UUID?
    private var detailTask: Task<Void, Never>?
    @Published private(set) var undeliveredResults: [GenerationRecord] = []
    @Published private(set) var devices: [DeviceSummary] = []
    @Published private(set) var legacyEnrollmentClosed = false
    @Published private(set) var isLoadingHistory = false
    @Published private(set) var hasMoreHistory = false
    @Published private(set) var historySourceFilter = "all"
    private var historyCursor: String?
    private(set) lazy var wisprImport = WisprFlowImportDriver(
        makeClient: { [weak self] in
            guard let self else { throw CancellationError() }
            return try client()
        },
        isDictationBusy: { [weak self] in self?.isBusy ?? true },
        endpoint: { [weak self] in self?.activeEndpoint ?? "" },
        onHistoryChanged: { [weak self] isCurrentRevision in
            guard let self else { return }
            if isCurrentRevision, historySourceFilter != "wispr-flow" {
                setHistorySourceFilter("wispr-flow")
            } else {
                // A cancelled run can still have committed on the server.
                // Refresh the user's current view without changing its filter.
                refreshHistory()
            }
        }
    )
    let configuration: ConfigurationStore
    let microphones: MicrophonePreferencesStore
    let preferences: ClientPreferencesStore
    private let pendingStore: PendingGenerationsStore
    private var pendingEntries: [PendingGenerationEntry]
    private var isReconcilingPending = false

    var isRecording: Bool { activity == .recording }
    var isCapturing: Bool { activity.isCapturing }
    var recordingUsesClipboard: Bool { isCapturing && insertionDestination == .clipboard }
    var isBusy: Bool { activity.isBusy || !pendingDictations.isEmpty }
    var pendingDictationCount: Int { pendingDictations.count }
    var canCancelWithEscape: Bool { isCapturing && !hotkey.isHoldingFn }
    var triggerKeyTitle: String {
        if let customTriggerKeyCode, let title = HoldKey.customKeyTitle(CGKeyCode(customTriggerKeyCode)) {
            return title
        }
        return shortcut.title
    }
    var isServerReady: Bool { serverHealth?.ready == true && serverHealth?.apiVersion == InlayAPI.version }
    var isUsingFallback: Bool { activeEndpoint != preferences.endpoints.first }
    var canTest: Bool { isServerReady && permissions.microphone && microphones.resolution.device != nil && !isCapturing }
    var insertionMode: InsertionMode { Self.insertionMode(for: permissions) }
    static func insertionMode(for snapshot: PermissionSnapshot) -> InsertionMode {
        if snapshot.accessibility { return .insertion }
        if snapshot.inputMonitoring { return .clipboardOnly }
        return .unavailable
    }
    var selectedInputName: String { microphones.resolution.device?.name ?? "No microphone available" }
    var allPermissionsGranted: Bool { permissions.microphone && permissions.accessibility }
    var onHUDVisibility: ((Bool) -> Void)?
    var onShowWindow: (() -> Void)?

    private let recorder: AudioRecorder
    private let audioDevices: AudioDeviceStore
    private let serverClientFactory: (() throws -> ServerClient)?
    private let hotkey = HotkeyMonitor()
    private let outputMuter: SystemOutputMuter
    private var subscriptions: Set<AnyCancellable> = []
    private var applyingConfiguration = false
    private var recordingTimer: Timer?
    private var recordingStart: TimeInterval = 0
    private var microphoneStartTask: Task<Void, Never>?
    private var recorderStopTask: Task<CapturedAudio, Error>?
    private var deliveryTail: Task<Void, Never>?
    private var insertionRebases = ConfirmedInsertionRebases<InsertionTarget>()
    /// Released takes in recording order. The newest may own the HUD via sessionID.
    @Published private var pendingDictations: [PendingDictation] = []
    private var uploadTask: Task<UploadOutcome, Error>?
    private var uploadPipe: AudioChunkPipe?
    private var refreshTask: Task<Void, Never>?
    private var queuedCheck: Bool?
    private var dataTask: Task<Void, Never>?
    private var monitorTask: Task<Void, Never>?
    private var pathMonitor: NWPathMonitor?
    private var hudTask: Task<Void, Never>?
    private var permissionTask: Task<Void, Never>?
    private var shortcutCheckTask: Task<Void, Never>?
    private var shortcutCheckStarted: TimeInterval = 0
    private var shortcutCheckEntries: [String] = []
    private var sessionID = UUID()
    private var activeGenerationID: UUID?
    private var activeClient: ServerClient?
    @MainActor
    private final class PendingDictation {
        let session: UUID
        let id: UUID
        let client: ServerClient
        let upload: Task<UploadOutcome, Error>
        let pipe: AudioChunkPipe
        let destination: InsertionDestinationCapture?
        let salvaged: Bool
        var task: Task<Void, Never>?
        var sealed = false
        var resolved = false
        /// Nil until the destination resolves; list continuation keys off its anchor.
        var target: (destination: InsertionDestination, anchor: DictationDestination?)?

        init(session: UUID, id: UUID, client: ServerClient, upload: Task<UploadOutcome, Error>,
             pipe: AudioChunkPipe, destination: InsertionDestinationCapture?, salvaged: Bool = false) {
            self.session = session; self.id = id; self.client = client; self.upload = upload
            self.pipe = pipe; self.destination = destination; self.salvaged = salvaged
        }

        func cancel() {
            task?.cancel(); upload.cancel(); pipe.cancel(); destination?.cancel()
        }
    }
    @Published private var insertionDestination: InsertionDestination?
    private var destinationTask: InsertionDestinationCapture?
    private var recordingClipboardChangeCount = 0
    private struct ContinuationAnchor {
        let server: String
        let destination: DictationDestination
        let generationID: UUID
        let continuation: DictationContinuation
        let timestamp: TimeInterval
    }
    private var continuationAnchors: [ContinuationAnchor] = []
    private var isTestSession = false
    private var sessionClipboardOnly = false
    private var updatingLogin = false
    private var hasInitialized = false
    private var isShuttingDown = false
    private var observers: [NSObjectProtocol] = []
    private var workspaceObservers: [NSObjectProtocol] = []
    private var lockObserver: NSObjectProtocol?

    init(configuration: ConfigurationStore, startServices: Bool = true,
         recorder: AudioRecorder? = nil, audioDevices: AudioDeviceStore? = nil,
         serverClient: (() throws -> ServerClient)? = nil) {
        self.configuration = configuration
        outputMuter = SystemOutputMuter(
            recordURL: configuration.url.deletingLastPathComponent().appendingPathComponent("output-mute.json"))
        self.recorder = recorder ?? AudioRecorder()
        self.audioDevices = audioDevices ?? AudioDeviceStore()
        self.serverClientFactory = serverClient
        preferences = ClientPreferencesStore(root: configuration.url.deletingLastPathComponent())
        activeEndpoint = preferences.endpoints.first ?? ClientPreferencesStore.defaultEndpoint
        let loadedPendingStore = PendingGenerationsStore(root: configuration.url.deletingLastPathComponent())
        pendingStore = loadedPendingStore
        pendingEntries = loadedPendingStore.load()
        microphones = MicrophonePreferencesStore(configuration: configuration)
        permissions = startServices ? PermissionSnapshot.capture()
            : PermissionSnapshot(microphone: false, accessibility: false, inputMonitoring: false)
        applyConfiguration(configuration.configuration)
        hotkey.key = shortcut
        microphones.objectWillChange.sink { [weak self] _ in self?.objectWillChange.send() }.store(in: &subscriptions)
        preferences.objectWillChange.sink { [weak self] _ in self?.objectWillChange.send() }.store(in: &subscriptions)
        configuration.$configuration.removeDuplicates().sink { [weak self] in self?.applyConfiguration($0) }.store(in: &subscriptions)
        guard startServices else { return }
        outputMuter.restorePersistedMute()
        Task { [weak self] in await self?.localServer.detect() }
        bindServices()
        self.audioDevices.start()
        installLifecycleObservers()
        startPathMonitor()
        refreshPermissions()
        // Capture files are temporary only; server history is never examined here.
        CapturedAudio.cleanupOrphans()
        try? FileManager.default.removeItem(at: InlayTemporaryDirectory.url.appendingPathComponent("Inlay-remote-preview"))
        hasInitialized = true
        updateLoginItem()
        refreshServer()
        monitorTask = Task { [weak self] in
            var count = 0
            while !Task.isCancelled {
                do { try await Task.sleep(for: .seconds(5)) } catch { return }
                guard let self, !isShuttingDown else { return }
                await checkServer(refreshData: count % 3 == 0 && !isBusy)
                count += 1
            }
        }
    }

    private func applyConfiguration(_ settings: InlayConfiguration) {
        guard !isBusy, !isShuttingDown else { return }
        applyingConfiguration = true
        if let key = HoldKey(rawValue: settings.holdKey), shortcut != key { shortcut = key }
        if let mode = HotkeyActivationMode(rawValue: settings.activationMode), activationMode != mode { activationMode = mode }
        if launchAtLogin != settings.launchAtLogin { launchAtLogin = settings.launchAtLogin }
        if muteOutputWhileRecording != settings.muteOutputWhileRecording { muteOutputWhileRecording = settings.muteOutputWhileRecording }
        if dictationSoundsEnabled != settings.dictationSoundsEnabled { dictationSoundsEnabled = settings.dictationSoundsEnabled }
        if customTriggerKeyCode != settings.customTriggerKeyCode { customTriggerKeyCode = settings.customTriggerKeyCode }
        if preserveClipboard != settings.preserveClipboard { preserveClipboard = settings.preserveClipboard }
        applyingConfiguration = false
    }

    private func client(for endpoint: String? = nil, session: URLSession? = nil) throws -> ServerClient {
        let endpoint = endpoint ?? activeEndpoint
        return try serverClientFactory?()
            ?? ServerClient(endpoint: endpoint, token: preferences.token(for: endpoint), session: session)
    }

    func refreshServer() {
        refreshTask = Task { [weak self] in await self?.checkServer(refreshData: true) }
    }

    /// Checks every listed server in parallel, then picks the active one.
    /// History loads and pending recovery run separately, so a slow server
    /// never delays failover or the dictation key.
    private func checkServer(refreshData: Bool) async {
        guard !isShuttingDown else { return }
        guard !isCheckingServer else {
            // A save or network change during a check must still be seen.
            queuedCheck = (queuedCheck ?? false) || refreshData
            return
        }
        isCheckingServer = true
        defer {
            isCheckingServer = false
            if let queued = queuedCheck {
                queuedCheck = nil
                refreshTask = Task { [weak self] in await self?.checkServer(refreshData: queued) }
            }
        }
        let endpoints = preferences.endpoints
        let probes = await probeServers(endpoints)
        guard endpoints == preferences.endpoints, !Task.isCancelled, !isShuttingDown else { return }
        serverProbes = probes
        if !isBusy {
            var enrolled = false
            for endpoint in endpoints where probes[endpoint]?.needsEnrollment == true {
                if await enrollThisDevice(at: endpoint) { enrolled = true }
            }
            if enrolled { queuedCheck = true; return }
        }
        if !applyProbes(), refreshData { refreshServerData() }
        Task { [weak self] in await self?.reconcilePendingGenerations() }
    }

    private func probeServers(_ endpoints: [String]) async -> [String: ServerProbe] {
        let connections = endpoints.map { endpoint in
            (endpoint, Result { try serverClientFactory?() ?? ServerClient(
                endpoint: endpoint, token: preferences.token(for: endpoint), session: ServerClient.probeSession) })
        }
        return await withTaskGroup(of: (String, ServerProbe).self) { group in
            for (endpoint, connection) in connections {
                group.addTask {
                    do { return (endpoint, ServerProbe(health: try await connection.get().health())) }
                    catch { return (endpoint, ServerProbe(error: error)) }
                }
            }
            var probes: [String: ServerProbe] = [:]
            for await (endpoint, probe) in group { probes[endpoint] = probe }
            return probes
        }
    }

    /// Moves to the first ready server unless a take is capturing, then
    /// updates status. Every switch happens here and reloads the new server's
    /// data. Returns true when the active server changed.
    @discardableResult
    private func applyProbes() -> Bool {
        let endpoints = preferences.endpoints
        let preferred = ServerFailover.preferred(endpoints, probes: serverProbes)
        var target = endpoints.contains(activeEndpoint) ? activeEndpoint : endpoints.first ?? activeEndpoint
        if !isCapturing, let preferred { target = preferred }
        let switched = target != activeEndpoint
        if switched {
            activeEndpoint = target
            resetServerData()
            refreshServerData()
        }
        let probe = serverProbes[activeEndpoint]
        serverHealth = probe?.health
        if let probe {
            if probe.isReady {
                serverStatusMessage = activeEndpoint == endpoints.first ? "Server online"
                    : "Using fallback: \(ServerFailover.displayName(activeEndpoint))"
            } else if let preferred {
                // Only while capturing; otherwise the switch above has already happened.
                serverStatusMessage = "Switching to \(ServerFailover.displayName(preferred)) after this take"
            } else if endpoints.count > 1 {
                serverStatusMessage = "No server is ready · Recording unavailable"
            } else {
                serverStatusMessage = probe.problem ?? "Server models are not ready"
            }
        }
        if !isBusy, activity == .idle { statusMessage = isServerReady ? "Ready when you are" : serverStatusMessage }
        return switched
    }

    /// History, shared settings, devices and list continuation all belong to one server.
    private func resetServerData() {
        continuationAnchors.removeAll()
        dataTask?.cancel()
        detailTask?.cancel()
        sharedPreferences = nil
        storageUsage = nil
        generations = []
        generationDetail = nil
        generationDetailFailedID = nil
        historyCursor = nil
        hasMoreHistory = false
        devices = []
        legacyEnrollmentClosed = false
    }

    private func refreshServerData() {
        let endpoint = activeEndpoint
        guard serverProbes[endpoint]?.isReachable == true else { return }
        dataTask?.cancel()
        dataTask = Task { [weak self] in
            guard let self else { return }
            do {
                let connection = try client(for: endpoint)
                let source = historySourceFilter
                async let settings = connection.preferences()
                async let page = connection.history(source: source == "all" ? nil : source)
                let (saved, history) = try await (settings, page)
                guard endpoint == activeEndpoint, source == historySourceFilter, !Task.isCancelled else { return }
                sharedPreferences = saved
                if generations.count > history.items.count, history.nextCursor != nil,
                   let oldest = history.items.last?.createdAt {
                    let ids = Set(history.items.map(\.id))
                    let older = generations.filter { $0.createdAt < oldest && !ids.contains($0.id) }
                    generations = history.items + older
                } else {
                    generations = history.items
                    historyCursor = history.nextCursor
                    hasMoreHistory = history.nextCursor != nil
                }
                await refreshDevices()
            } catch {}
        }
    }

    func saveConnection(servers: [(endpoint: String, token: String)], deviceName: String) {
        guard !isBusy, !wisprImport.isImportRunning else { return }
        guard preferences.save(servers: servers, deviceName: deviceName) else {
            errorMessage = preferences.errorMessage
            return
        }
        // Renaming, reordering or re-entering a token keeps results owed by servers still listed.
        pendingEntries.removeAll { !preferences.endpoints.contains($0.endpoint) }
        undeliveredResults.removeAll { result in !pendingEntries.contains { $0.id == result.id } }
        pendingStore.save(pendingEntries)
        activeEndpoint = preferences.endpoints.first ?? ClientPreferencesStore.defaultEndpoint
        resetServerData()
        serverProbes = [:]
        serverHealth = nil
        serverStatusMessage = "Connecting…"
        refreshServer()
    }

    func refreshHistory() {
        Task { [weak self] in
            guard let self else { return }
            do {
                let endpoint = activeEndpoint
                let source = historySourceFilter
                let page = try await client().history(source: source == "all" ? nil : source)
                guard endpoint == activeEndpoint, source == historySourceFilter else { return }
                generations = page.items
                historyCursor = page.nextCursor
                hasMoreHistory = page.nextCursor != nil
            } catch { errorMessage = error.localizedDescription }
        }
    }

    func loadMoreHistory() {
        guard !isLoadingHistory, let cursor = historyCursor else { return }
        isLoadingHistory = true
        Task { [weak self] in
            guard let self else { return }
            defer { isLoadingHistory = false }
            do {
                let endpoint = activeEndpoint
                let source = historySourceFilter
                let page = try await client().history(before: cursor, source: source == "all" ? nil : source)
                guard endpoint == activeEndpoint, historyCursor == cursor, source == historySourceFilter else { return }
                let existing = Set(generations.map(\.id))
                generations += page.items.filter { !existing.contains($0.id) }
                historyCursor = page.nextCursor
                hasMoreHistory = page.nextCursor != nil
            } catch { errorMessage = error.localizedDescription }
        }
    }

    func setHistorySourceFilter(_ source: String) {
        guard ["all", "inlay", "wispr-flow"].contains(source), source != historySourceFilter else { return }
        historySourceFilter = source
        generations = []
        historyCursor = nil
        hasMoreHistory = false
        refreshHistory()
    }

    func updateSharedPreferences(_ value: ServerPreferences, expectedRevision: Int? = nil) {
        guard !isSavingPreferences, let snapshot = sharedPreferences else { return }
        isSavingPreferences = true
        Task { [weak self] in
            guard let self else { return }
            defer { isSavingPreferences = false }
            do {
                let endpoint = activeEndpoint
                let saved = try await client().updatePreferences(.init(revision: expectedRevision ?? snapshot.revision, preferences: value))
                guard endpoint == activeEndpoint else { return }
                sharedPreferences = saved
                errorMessage = nil
            } catch { errorMessage = error.localizedDescription; refreshServer() }
        }
    }

    private func enrollThisDevice(at endpoint: String) async -> Bool {
        guard let connection = try? client(for: endpoint) else { return false }
        do {
            let response = try await connection.enroll(
                device: DeviceIdentity(id: preferences.deviceID, name: preferences.deviceName))
            // A server removed mid-enroll must not have its new token stored.
            guard preferences.endpoints.contains(endpoint) else { return false }
            return preferences.replaceToken(response.token, for: endpoint)
        } catch { return false }
    }

    func refreshDevices() async {
        let endpoint = activeEndpoint
        guard let connection = try? client(), let list = try? await connection.devices(),
              endpoint == activeEndpoint else { return }
        devices = list.devices
        legacyEnrollmentClosed = list.legacyEnrollmentClosed
    }

    func revokeDevice(_ id: String) {
        Task { [weak self] in
            guard let self else { return }
            do {
                try await client().revokeDevice(id)
                await refreshDevices()
            } catch { errorMessage = error.localizedDescription }
        }
    }

    func closeLegacyEnrollment() {
        Task { [weak self] in
            guard let self else { return }
            do {
                let list = try await client().closeLegacyEnrollment()
                devices = list.devices
                legacyEnrollmentClosed = list.legacyEnrollmentClosed
            } catch { errorMessage = error.localizedDescription }
        }
    }

    func transcribeAgain(_ id: UUID) {
        Task { [weak self] in
            guard let self else { return }
            do {
                _ = try await client().reprocess(id)
                refreshServer()
            } catch { errorMessage = error.localizedDescription }
        }
    }

    func refreshStorageUsage() async {
        guard let connection = try? client(), let usage = try? await connection.storageUsage() else { return }
        storageUsage = usage
    }

    func purgeAudio(kind: AudioKind?) {
        Task { [weak self] in
            guard let self else { return }
            do {
                _ = try await client().purgeAudio(PurgeAudioRequest(kind: kind))
                // Purges leave summaries unchanged, so the open detail reloads here.
                if let id = generationDetail?.id { loadGenerationDetail(id) }
                await refreshStorageUsage()
                refreshHistory()
            } catch { errorMessage = error.localizedDescription }
        }
    }

    func deleteGenerationAudio(_ id: UUID) {
        Task { [weak self] in
            guard let self else { return }
            do {
                try await client().deleteAudio(id)
                if generationDetail?.id == id { generationDetail = nil; loadGenerationDetail(id) }
                refreshHistory()
                await refreshStorageUsage()
            } catch { errorMessage = error.localizedDescription }
        }
    }

    /// Connects this Mac to the app-managed local server after install:
    /// saves the endpoint + admin token, then the normal health flow
    /// auto-enrolls this Mac (stack 05) on the next refresh.
    @discardableResult
    func completeLocalSetup() -> Bool {
        guard !isBusy, let token = localServer.adminToken() else { return false }
        let local = ClientPreferencesStore.defaultEndpoint
        // This Mac's server is the fallback: it keeps its place, or goes last.
        var servers = preferences.endpoints.map { endpoint in
            (endpoint: endpoint, token: endpoint == local ? token : preferences.token(for: endpoint))
        }
        if !preferences.endpoints.contains(local) { servers.append((endpoint: local, token: token)) }
        saveConnection(servers: servers, deviceName: preferences.deviceName)
        guard preferences.token(for: local) == token else { return false }
        refreshServer()
        return true
    }

    func uninstallLocalServer() {
        localServer.uninstall()
        refreshServer()
    }

    /// Loads the full record for the history detail pane (summaries stay in the list).
    /// Reloading the shown record keeps it on screen until the fresh copy arrives.
    func loadGenerationDetail(_ id: UUID?) {
        detailTask?.cancel()
        if generationDetail?.id != id { generationDetail = nil }
        generationDetailFailedID = nil
        guard let id else { return }
        detailTask = Task { [weak self] in
            guard let self else { return }
            do {
                let record = try await client().generation(id)
                guard !Task.isCancelled else { return }
                generationDetail = record
            } catch is CancellationError {
            } catch {
                guard !Task.isCancelled else { return }
                errorMessage = error.localizedDescription
                generationDetailFailedID = id
            }
        }
    }

    func deleteGeneration(_ id: UUID) {
        Task { [weak self] in
            guard let self else { return }
            do {
                try await client().delete(id)
                generations.removeAll { $0.id == id }
                continuationAnchors.removeAll { $0.generationID == id }
            } catch { errorMessage = error.localizedDescription }
        }
    }

    func openGenerationAudio(_ generation: GenerationRecord, kind: AudioKind) {
        Task { [weak self] in
            guard let self else { return }
            do { NSWorkspace.shared.open(try await client().audio(generation.id, kind: kind)) }
            catch { errorMessage = error.localizedDescription }
        }
    }

    func openWisprFlowArtifact(_ generation: GenerationRecord, filename: WisprFlowArtifactName) {
        guard generation.importedSource?.artifactNames.contains(filename) == true else { return }
        Task { [weak self] in
            guard let self else { return }
            do { NSWorkspace.shared.open(try await client().wisprFlowArtifact(generation.id, filename: filename)) }
            catch { errorMessage = error.localizedDescription }
        }
    }

    private static func connectionMessage(_ error: Error) -> String { ServerProbe.message(for: error) }

    func toggleTestRecording() {
        guard !hotkey.isHoldingFn else { return }
        if isCapturing { finishDictation() }
        else { beginDictation(isTest: true) }
    }

    func toggleMenuDictation() {
        guard !hotkey.isHoldingFn else { return }
        if isCapturing { finishDictation() }
        else { beginDictation(isTest: false) }
    }

    func cancelDictation() {
        guard isBusy else { return }
        if isCapturing {
            let generation = activeGenerationID
            let connection = activeClient
            resetSession()
            if let generation, let connection { Task { try? await connection.cancel(generation) } }
        } else if activity.isBusy, let index = pendingDictations.lastIndex(where: { $0.session == sessionID }) {
            let pending = pendingDictations.remove(at: index)
            pending.cancel()
            Task { try? await pending.client.cancel(pending.id) }
        } else if let earlier = pendingDictations.last {
            // The HUD shows a settled result or error. Dismissing it must not
            // cancel older work that the HUD does not show.
            errorMessage = nil
            showEarlierDictation(earlier)
            return
        }
        let remaining = pendingDictations.last?.session
        sessionID = remaining ?? UUID()
        activity = remaining == nil ? .idle : .transcribing
        if remaining == nil { hudProgressText = nil }
        statusMessage = remaining == nil ? "Cancelled" : "Cancelled · Earlier dictation is still processing"
        errorMessage = nil
        onHUDVisibility?(remaining != nil)
        refreshServer()
    }

    private func showEarlierDictation(_ pending: PendingDictation) {
        hudTask?.cancel()
        sessionID = pending.session
        activity = .transcribing
        statusMessage = "Earlier dictation is still processing"
        onHUDVisibility?(true)
    }

    private func resetSession() {
        sessionID = UUID()
        hotkey.clearLatchedTake()
        microphoneStartTask?.cancel(); microphoneStartTask = nil
        uploadPipe?.cancel(); uploadPipe = nil
        uploadTask?.cancel(); uploadTask = nil
        destinationTask?.cancel(); destinationTask = nil
        stopRecordingTimer()
        recorder.cancel()
        outputMuter.restore()
        recorder.onChunk = nil
        insertionDestination = nil
        recordingListHint = nil
        recordingInputName = nil
        activeGenerationID = nil
        activeClient = nil
        recordingFeedback.reset()
    }

    private func failSession(_ message: String, cancelServer: Bool) {
        let generation = activeGenerationID
        let connection = activeClient
        resetSession()
        showError(message)
        if cancelServer, let generation, let connection { Task { try? await connection.cancel(generation) } }
        refreshHistory()
    }

    private func addPendingEntry(id: UUID, endpoint: String, finish: FinishGenerationRequest,
                                 fallback: FinishGenerationRequest?, isTest: Bool) {
        pendingEntries.removeAll { $0.id == id }
        pendingEntries.append(PendingGenerationEntry(
            id: id, endpoint: endpoint,
            inferenceFrames: finish.inferenceFrames, originalFrames: finish.originalFrames,
            fallbackInferenceFrames: fallback?.inferenceFrames, isTest: isTest, sealedAt: Date()))
        pendingStore.save(pendingEntries)
    }

    /// A lost upload acknowledgement leaves the server one chunk ahead of this
    /// Mac's count, which `finish` rejects as incomplete. Seal with the
    /// unacknowledged count instead of cancelling speech the server holds.
    private static func seal(_ connection: ServerClient, id: UUID, finish: FinishGenerationRequest,
                             fallback: FinishGenerationRequest?) async throws -> GenerationRecord {
        do { return try await connection.finish(id, value: finish) }
        catch {
            guard let fallback, let clientError = error as? ServerClientError,
                  case .rejected(409, _, "incomplete_audio") = clientError else { throw error }
            return try await connection.finish(id, value: fallback)
        }
    }

    private func removePendingEntry(_ id: UUID) {
        pendingEntries.removeAll { $0.id == id }
        undeliveredResults.removeAll { $0.id == id }
        pendingStore.save(pendingEntries)
    }

    /// Surfaces takes that finished on the server while this app was not
    /// tracking them. Completed results are copy-only here: never inserted.
    private func reconcilePendingGenerations() async {
        guard !isReconcilingPending, !pendingEntries.isEmpty else { return }
        isReconcilingPending = true
        defer { isReconcilingPending = false }
        let cutoff = Date().addingTimeInterval(-7 * 24 * 60 * 60)
        let stale = pendingEntries.filter { $0.sealedAt < cutoff }
        for entry in stale { removePendingEntry(entry.id) }
        // Takes this session still owns deliver themselves; never offer them for copy.
        func isLive(_ id: UUID) -> Bool { pendingDictations.contains { $0.id == id } }
        // A connection change during the read drops entries; skip those.
        func isCurrent(_ entry: PendingGenerationEntry) -> Bool {
            preferences.endpoints.contains(entry.endpoint) && pendingEntries.contains { $0.id == entry.id }
        }
        // Each take is recovered from the server that holds it, once that server answers.
        for entry in pendingEntries where isCurrent(entry) && !isLive(entry.id)
            && serverProbes[entry.endpoint]?.isReachable == true {
            guard let connection = try? client(for: entry.endpoint),
                  let record = try? await connection.generation(entry.id),
                  isCurrent(entry), !isLive(entry.id) else { continue }
            switch record.status {
            case .completed:
                if record.delivery == nil {
                    if !undeliveredResults.contains(where: { $0.id == record.id }) {
                        undeliveredResults.append(record)
                    }
                } else { removePendingEntry(entry.id) }
            case .failed, .cancelled:
                removePendingEntry(entry.id)
            case .receiving:
                do {
                    _ = try await Self.seal(connection, id: entry.id, finish: FinishGenerationRequest(
                        inferenceFrames: entry.inferenceFrames, originalFrames: entry.originalFrames),
                        fallback: entry.fallbackInferenceFrames.map { .init(inferenceFrames: $0, originalFrames: nil) })
                } catch let ServerClientError.rejected(status, _, _) where (400..<500).contains(status) && ![401, 403, 408, 429].contains(status) {
                    // Auth failures may come from a credential the user has just
                    // replaced; they do not prove the speech is unrecoverable.
                    if isCurrent(entry) { removePendingEntry(entry.id) }
                } catch {}
            case .queued, .transcribing, .proofreading:
                break
            }
        }
    }

    func copyUndeliveredResult(_ record: GenerationRecord) {
        let endpoint = pendingEntries.first { $0.id == record.id }?.endpoint
        switch DictationClipboard.copy(record.finalText, to: .general) {
        case .success:
            lastTranscript = record.finalText
            lastDelivery = "Copied to clipboard"
            lastDeliveryStatus = .copied
            removePendingEntry(record.id)
            Task { [weak self] in
                guard let endpoint, let connection = try? self?.client(for: endpoint) else { return }
                try? await connection.delivery(record.id, receipt: DeliveryReceipt(
                    status: "copied", message: "Copied after relaunch"))
            }
        case .failure(let error):
            errorMessage = error.localizedDescription
        }
    }

    func copyLastTranscript() {
        guard !lastTranscript.isEmpty, !isBusy else { return }
        switch DictationClipboard.copy(lastTranscript, to: .general) {
        case .success: lastDelivery = "Copied to clipboard"; lastDeliveryStatus = .copied
        case .failure(let error): lastDelivery = error.localizedDescription; lastDeliveryStatus = .failed
        }
    }

    func clearLastTranscript() {
        guard !isBusy else { return }
        continuationAnchors.removeAll()
        lastTranscript = ""; lastTranscriptionSeconds = nil; lastAudioSeconds = nil
        lastDelivery = ""; lastDeliveryStatus = .none; errorMessage = nil; activity = .idle; attentionPresented = false
        recordingFeedback.reset()
    }

    func dismissFeedback() {
        guard !isBusy else { return }
        hudTask?.cancel()
        attentionPresented = false
        recordingFeedback.reset()
        onHUDVisibility?(false)
        if activity == .success || activity == .failed {
            activity = .idle
            statusMessage = isServerReady ? "Ready when you are" : serverStatusMessage
        }
    }

    func shutdown() {
        guard !isShuttingDown else { return }
        isShuttingDown = true
        wisprImport.shutdown()
        stopShortcutCheck()
        // Quitting the client cancels an incomplete recording. A sealed server
        // generation remains independently owned and may complete in history.
        let generation = activeGenerationID
        let connection = activeClient
        resetSession()
        if let generation, let connection { Task { try? await connection.cancel(generation) } }
        stopPendingDictations()
        monitorTask?.cancel(); refreshTask?.cancel(); dataTask?.cancel(); hudTask?.cancel(); permissionTask?.cancel()
        pathMonitor?.cancel()
        configuration.stopWatching()
        subscriptions.removeAll()
        audioDevices.stop(); hotkey.stop(); continuationAnchors.removeAll()
        for observer in observers { NotificationCenter.default.removeObserver(observer) }
        for observer in workspaceObservers { NSWorkspace.shared.notificationCenter.removeObserver(observer) }
        if let lockObserver { DistributedNotificationCenter.default().removeObserver(lockObserver) }
        try? FileManager.default.removeItem(at: InlayTemporaryDirectory.url.appendingPathComponent("Inlay-remote-preview"))
    }

    private func bindServices() {
        audioDevices.onChange = { [weak self] devices, defaultUID in self?.microphones.update(devices: devices, systemDefaultUID: defaultUID) }
        recorder.onLevel = { [weak self] level in guard let self, isCapturing else { return }; recordingFeedback.append(level) }
        recorder.onInterruption = { [weak self] message in self?.salvageDictation(reason: message) }
        hotkey.onStatusChange = { [weak self] in self?.isHotkeyActive = $0 }
        hotkey.onPress = { [weak self] in
            guard let self else { return false }
            if isCheckingShortcut {
                appendShortcutCheck("Shortcut recognized. Recording was intentionally skipped.")
                // No take started, so a double-tap monitor must not latch.
                return false
            }
            return beginDictation(isTest: false)
        }
        hotkey.onRelease = { [weak self] in
            guard let self else { return }
            if isCheckingShortcut { appendShortcutCheck("Hold released."); return }
            if !isTestSession { finishDictation() }
        }
        hotkey.onCancel = { [weak self] in
            guard let self else { return }
            if isCheckingShortcut { appendShortcutCheck("Hold cancelled; microphone stayed off.") }
            else if canCancelWithEscape { cancelDictation() }
            else if !isBusy { dismissFeedback() }
        }
    }

    /// Returns whether a take actually started. A double-tap monitor latches
    /// only on true, so a rejected start cannot leave a phantom recording.
    @discardableResult
    private func beginDictation(isTest: Bool) -> Bool {
        guard !isCapturing, !isShuttingDown else { return false }
        permissions = PermissionSnapshot.capture()
        stopShortcutCheck()
        guard isServerReady else { showError(serverStatusMessage); refreshServer(); return false }
        guard permissions.microphone else { showError("Allow microphone access, then try again."); return false }
        guard let input = microphones.resolution.device, let deviceID = audioDevices.deviceID(for: input.uid) else {
            showError("No microphone is available. Connect an input and try again."); return false
        }
        guard isTest || Self.insertionMode(for: permissions) != .unavailable else {
            showError("Allow Accessibility or Input Monitoring access, then try again."); return false
        }
        hudTask?.cancel(); errorMessage = nil; attentionPresented = false
        // Confirmed cursor moves matter only to takes that overlap them.
        if pendingDictations.isEmpty { insertionRebases.removeAll() }
        sessionID = UUID()
        let current = sessionID
        isTestSession = isTest
        sessionClipboardOnly = !isTest && Self.insertionMode(for: permissions) == .clipboardOnly
        recordingClipboardChangeCount = NSPasteboard.general.changeCount
        insertionDestination = nil
        recordingInputName = input.name
        recordingFeedback.reset()
        activity = .starting
        statusMessage = "Connecting recording…"
        onHUDVisibility?(true)
        if muteOutputWhileRecording { outputMuter.mute() }
        if !isTest, !sessionClipboardOnly {
            let capture = TextInserter.beginDestinationCapture()
            destinationTask = capture
            Task { [weak self] in
                let destination = await capture.value
                guard let self, sessionID == current, isCapturing else { return }
                insertionDestination = destination
                prepareContinuation(for: destination.target.map(DictationDestination.field))
            }
        } else { prepareContinuation(for: .test) }
        microphoneStartTask = Task { [weak self] in
            guard let self else { return }
            defer { if sessionID == current { microphoneStartTask = nil } }
            do {
                let request = CreateGenerationRequest(requestID: current,
                    device: .init(id: preferences.deviceID, name: preferences.deviceName), mode: isTest ? .test : .dictation)
                var connection = try client()
                let created: GenerationRecord
                do {
                    // With another server ready, one that stopped answering must not hold the key for 12s.
                    let probes = serverProbes
                    let hasFallback = preferences.endpoints.contains {
                        $0 != connection.address && probes[$0]?.isReady == true
                    }
                    let admission = hasFallback
                        ? try client(for: connection.address, session: ServerClient.admissionSession) : connection
                    created = try await admission.create(request)
                } catch {
                    // Admission precedes the microphone, so one retry on the next
                    // ready server loses nothing. The switch itself waits for the take to end.
                    guard ServerProbe.isUnavailable(error), sessionID == current, !Task.isCancelled else { throw error }
                    serverProbes[connection.address] = ServerProbe(error: error)
                    applyProbes()
                    guard let next = ServerFailover.preferred(preferences.endpoints, probes: serverProbes) else { throw error }
                    connection = try client(for: next)
                    do { created = try await connection.create(request) }
                    catch {
                        // Mark it down too, so the next take does not pick it from a stale probe.
                        if ServerProbe.isUnavailable(error) { serverProbes[next] = ServerProbe(error: error) }
                        throw error
                    }
                }
                guard sessionID == current, activity == .starting, !Task.isCancelled else {
                    Task { try? await connection.cancel(created.id) }; return
                }
                guard created.status == .receiving else { throw ServerClientError.invalidResponse }
                activeGenerationID = created.id
                activeClient = connection
                // A retried take's settings belong to its fallback, not the server the pages show.
                if connection.address == activeEndpoint { sharedPreferences = created.settings }
                // A backlog stops capture; the upload seals what reached the server.
                let pipe = AudioChunkPipe { [weak self] error in
                    Task { @MainActor [weak self] in
                        guard let self else { return }
                        if sessionID == current, isCapturing {
                            salvageDictation(reason: error.localizedDescription)
                        }
                    }
                }
                uploadPipe = pipe
                recorder.onChunk = { pipe.append($0) }
                uploadTask = Task { [weak self] in
                    do {
                        let outcome = try await connection.upload(pipe.stream, to: created.id, preserveOriginal: created.settings.preferences.keepOriginalAudio)
                        if let self, !outcome.complete, sessionID == current, isCapturing, !Task.isCancelled {
                            salvageDictation(reason: "The connection was interrupted.")
                        }
                        return outcome
                    }
                    catch {
                        if let self, sessionID == current, isCapturing, !Task.isCancelled {
                            failSession(Self.connectionMessage(error), cancelServer: true)
                        }
                        throw error
                    }
                }
                // A released take owns its teardown. Never let its asynchronous
                // stop consume the new microphone request.
                if let recorderStopTask { _ = try? await recorderStopTask.value }
                guard sessionID == current, activity == .starting, !Task.isCancelled else { return }
                recordingStart = ProcessInfo.processInfo.systemUptime
                statusMessage = "Starting microphone…"
                try await recorder.start(deviceID: deviceID, preserveOriginalAudio: created.settings.preferences.keepOriginalAudio)
                guard sessionID == current, activity == .starting, !Task.isCancelled else { return }
                activity = .recording
                statusMessage = "Listening"
                announceOutcome("Listening", .start)
                startRecordingTimer()
            } catch is CancellationError {
            } catch AudioRecordingError.cancelled {
            } catch {
                guard sessionID == current, !Task.isCancelled else { return }
                serverHealth = nil
                serverStatusMessage = Self.connectionMessage(error)
                failSession(serverStatusMessage, cancelServer: true)
                refreshServer()
            }
        }
        return true
    }

    /// Interruption path: seal whatever the server already acknowledged and
    /// deliver to the clipboard only. The captured insertion target cannot be
    /// trusted after sleep, lock, or device loss, so this never auto-inserts.
    private func salvageDictation(reason: String) {
        guard isCapturing else { return }
        guard activity == .recording else { cancelDictation(); return }
        statusMessage = "Saving interrupted recording…"
        finishDictation(salvaged: true)
    }

    private func finishDictation(atLimit: Bool = false, salvaged: Bool = false) {
        guard isCapturing else { return }
        // A take ended by the duration limit must not leave a double-tap
        // latch behind, matching the failure and cancel paths.
        hotkey.clearLatchedTake()
        recorder.stopAcceptingAudio()
        outputMuter.restore()
        guard activity == .recording else { cancelDictation(); return }
        let releasedAt = ProcessInfo.processInfo.systemUptime
        destinationTask?.finish()
        guard releasedAt - recordingStart >= 0.25 else { cancelDictation(); showError("Too short — hold the key, speak, then release."); return }
        guard let id = activeGenerationID, let connection = activeClient, let uploadTask, let uploadPipe else {
            failSession("This recording has no server session.", cancelServer: true); return
        }
        stopRecordingTimer(); resetLevels()
        recordingFeedback.finish(atLimit: atLimit)
        activity = .transcribing
        statusMessage = "Finishing upload…"
        hudProgressText = "Uploading"
        let current = sessionID
        let test = isTestSession
        let capturedDestination = insertionDestination
        let clipboardCount = recordingClipboardChangeCount
        let pending = PendingDictation(session: current, id: id, client: connection, upload: uploadTask,
                                       pipe: uploadPipe, destination: destinationTask, salvaged: salvaged)
        // Usually known at release, so a later take can continue a list in another field.
        let knownDestination: InsertionDestination? = test || salvaged || sessionClipboardOnly ? .clipboard : capturedDestination
        pending.target = knownDestination.map { Self.resolve($0, isTest: test, releasedAt: releasedAt) }
        pendingDictations.append(pending)
        activeGenerationID = nil; activeClient = nil; self.uploadTask = nil; self.uploadPipe = nil
        destinationTask = nil; insertionDestination = nil; recordingListHint = nil; recordingInputName = nil
        recorder.onChunk = nil
        let stopped = recorder.stopCapture()
        recorderStopTask = stopped
        let precedingDelivery = deliveryTail
        let task = Task { [weak self] in
            guard let self else { return }
            var capturedAudio: CapturedAudio?
            defer {
                capturedAudio?.cleanup()
                if pending.resolved { removePendingEntry(pending.id) }
                pendingDictations.removeAll { $0 === pending }
                if pendingDictations.isEmpty {
                    deliveryTail = nil
                    applyConfiguration(configuration.configuration)
                }
            }
            do {
                let stoppedAudio: CapturedAudio?
                do { stoppedAudio = try await stopped.value }
                catch {
                    // A salvaged take lives in the streamed, server-acknowledged
                    // audio; a dead local capture must not discard it.
                    if salvaged { stoppedAudio = nil } else { throw error }
                }
                capturedAudio = stoppedAudio
                try Task.checkCancellation()
                pending.pipe.finish()
                let uploadOutcome = try await pending.upload.value
                var finish = uploadOutcome.finish
                var fallback = uploadOutcome.fallback
                let truncated = !uploadOutcome.complete && !test
                if !uploadOutcome.complete, max(finish.inferenceFrames, fallback?.inferenceFrames ?? 0) < 4_000 {
                    throw ServerClientError.rejected(422, "The interrupted recording was too short to save.", "")
                }
                if truncated {
                    // Released, but the upload stopped early: only a prefix exists.
                    // Like a salvaged take, it is copied and never auto-inserted.
                    pending.target = (.clipboard, nil)
                    if sessionID == current { statusMessage = "Saving interrupted recording…" }
                }
                try Task.checkCancellation()
                stoppedAudio?.cleanup()
                capturedAudio = nil
                let target: (destination: InsertionDestination, anchor: DictationDestination?)
                if let known = pending.target { target = known }
                else {
                    target = Self.resolve(await pending.destination?.value ?? .clipboard, isTest: test, releasedAt: releasedAt)
                    pending.target = target
                }
                let resolved = target.destination, anchor = target.anchor
                // Queued takes must not both extend the last delivered list snapshot.
                // An earlier take that is unresolved or shares this anchor suppresses it.
                let sharesEarlierAnchor = pendingDictations.prefix { $0 !== pending }
                    .contains { earlier in earlier.target.map { $0.anchor == anchor } ?? true }
                finish.continuationID = sharesEarlierAnchor ? nil
                    : anchor.flatMap { self.continuation(for: $0, server: connection.address)?.generationID }
                fallback?.continuationID = finish.continuationID
                try Task.checkCancellation()
                pending.sealed = true // The server may accept a seal whose response is interrupted.
                addPendingEntry(id: id, endpoint: connection.address, finish: finish, fallback: fallback, isTest: test)
                var result: GenerationRecord
                do { result = try await Self.seal(connection, id: id, finish: finish, fallback: fallback) }
                catch let ServerClientError.rejected(status, message, code) where (400..<500).contains(status) && ![408, 429].contains(status) {
                    pending.sealed = false // The server answered without sealing, so cancel promptly.
                    throw ServerClientError.rejected(status, message, code)
                }
                try Task.checkCancellation()
                if !result.status.isTerminal {
                    result = try await connection.events(id) { [weak self] record in
                        await self?.applyProgress(record, session: current)
                    }
                }
                try Task.checkCancellation()
                guard result.status == .completed else {
                    pending.resolved = true // Terminal on the server; visible in History.
                    throw ServerClientError.rejected(422, result.error ?? "The server could not process this recording.", "")
                }
                // Processing and uploads overlap. Clipboard/paste transactions
                // remain ordered and each keeps its original destination.
                await precedingDelivery?.value
                await waitForCaptureRelease()
                try Task.checkCancellation()
                let deliveryDestination = rebasedDestination(resolved)
                var receipt = await deliver(result, from: connection.address, to: deliveryDestination, anchor: anchor,
                                            isTest: test, clipboardCount: clipboardCount, session: current)
                if truncated, receipt.status == DictationDeliveryStatus.copied.rawValue {
                    let message = "Recording was interrupted. The saved part was copied to the clipboard."
                    receipt.message = message
                    lastDelivery = message
                }
                try Task.checkCancellation()
                do { try await connection.delivery(id, receipt: receipt) }
                catch { continuationAnchors.removeAll { $0.generationID == id } }
                try Task.checkCancellation()
                // Other takes can change the shared last-delivery state during the receipt request.
                let delivered = DictationDeliveryStatus(rawValue: receipt.status) ?? .failed
                if sessionID == current {
                    hudProgressText = nil
                    activity = delivered == .failed ? .failed : .success
                    if delivered.needsAttention {
                        attentionPresented = true
                        if delivered == .copied { dismissHUDAfter(seconds: 2, clearAttention: true) }
                    } else { dismissHUDAfter(seconds: 1.7) }
                    let outcomeSound: OutcomeFeedback.Sound =
                        switch delivered {
                        case .inserted, .tested, .listUpdated: .success
                        case .copied, .unconfirmed: .attention
                        case .failed, .none: .failure
                        }
                    announceOutcome(receipt.message ?? delivered.hudLabel, outcomeSound)
                } else if delivered.needsAttention {
                    // A newer take owns the HUD and will replace this result.
                    errorMessage = "Earlier dictation: \(receipt.message ?? delivered.hudLabel)"
                    if let errorMessage { announceOutcome(errorMessage, .attention) }
                }
                pending.resolved = true
                refreshServer()
            } catch {
                pending.upload.cancel(); pending.pipe.cancel(); pending.destination?.cancel()
                if !pending.sealed {
                    pending.resolved = true
                    Task { try? await connection.cancel(id) }
                }
                guard !Task.isCancelled else { return }
                if let clientError = error as? ServerClientError, case .processingTimedOut = clientError {
                    // Not a failure: stop blocking later deliveries and let the
                    // persisted-pending reconciliation surface the result.
                    if sessionID == current {
                        activity = .idle
                        statusMessage = "Still processing on the server"
                    }
                } else if sessionID == current {
                    // Only the take's own server is marked down; it may no longer be the active one.
                    if error is URLError { serverProbes[connection.address] = ServerProbe(error: error); applyProbes() }
                    showError(Self.connectionMessage(error))
                } else {
                    let message = "Earlier dictation failed: \(Self.connectionMessage(error))"
                    errorMessage = message
                    lastDelivery = message
                    lastDeliveryStatus = .failed
                    // An older job may fail while the microphone or a newer
                    // result owns the HUD. Keep that live activity intact.
                    if !activity.isBusy { showError(message) } else { announceOutcome(message, .failure) }
                }
                refreshHistory()
            }
        }
        pending.task = task
        // Even a failed/cancelled middle take must preserve the ordering link
        // to earlier deliveries for every later take.
        deliveryTail = Task {
            await precedingDelivery?.value
            await task.value
        }
    }

    private static func resolve(_ destination: InsertionDestination, isTest: Bool,
                                releasedAt: TimeInterval) -> (destination: InsertionDestination, anchor: DictationDestination?) {
        var resolved = destination
        if let target = destination.target, !InsertionCapturePolicy.permitsInsertion(capturedAt: target.capturedAt, releasedAt: releasedAt) {
            resolved = .clipboard
        }
        return (resolved, isTest ? .test : resolved.target.flatMap { $0.selection == nil ? nil : .field($0) })
    }

    private func applyProgress(_ generation: GenerationRecord, session: UUID) {
        guard sessionID == session, !isCapturing else { return }
        switch generation.status {
        case .receiving: statusMessage = "Finishing upload…"; hudProgressText = "Uploading"
        case .queued:
            if let position = generation.queuePosition {
                statusMessage = "Queued on server… (#\(position))"
                hudProgressText = "Queued #\(position)"
            } else {
                statusMessage = "Queued on server…"; hudProgressText = "Queued"
            }
        case .transcribing: statusMessage = "Transcribing on server…"; hudProgressText = "Transcribing"
        case .proofreading: statusMessage = "Proofreading on server…"; hudProgressText = "Proofreading"
        case .completed: statusMessage = "Preparing result…"; hudProgressText = "Preparing"
        case .failed, .cancelled: statusMessage = generation.error ?? "Processing stopped"; hudProgressText = nil
        }
    }

    private func deliver(_ record: GenerationRecord, from server: String, to destination: InsertionDestination,
                         anchor: DictationDestination?, isTest: Bool, clipboardCount: Int,
                         session: UUID) async -> DeliveryReceipt {
        var transcript = record.previewText.isEmpty ? record.finalText : record.previewText
        let message: String
        let deliveryStatus: DictationDeliveryStatus
        let status: String
        if isTest {
            rememberContinuation(record, from: server, at: .test)
            message = "Test complete. Nothing was pasted."
            deliveryStatus = .tested
            status = record.finalText.isEmpty ? "No speech detected" : "Ready to copy"
        } else if record.insertionText.isEmpty {
            if record.continuation == nil && record.previewText.isEmpty {
                message = "No speech detected"; deliveryStatus = .none
            } else if let confirmed = TextInserter.unchangedAnchor(destination.target) {
                rememberContinuation(record, from: server, at: .field(confirmed))
                message = "List updated. Nothing was pasted."; deliveryStatus = .listUpdated
            } else {
                message = "List state unchanged: the original cursor could not be confirmed."
                deliveryStatus = .unconfirmed
            }
            status = message
        } else {
            if sessionID == session { activity = .delivering; statusMessage = "Inserting at your cursor…"; hudProgressText = "Inserting" }
            let inserter = TextInserter()
            let outcome = await inserter.deliver(record.insertionText, copying: record.finalText,
                                                  to: destination, clipboardUnchangedSince: clipboardCount,
                                                  waitUntilReady: { await self.waitForCaptureRelease() },
                                                  isCaptureActive: { self.isHoldingCapture },
                                                  preservingClipboard: preserveClipboard)
            guard !Task.isCancelled else { return DeliveryReceipt(status: "failed", message: "Delivery cancelled") }
            if let anchor { continuationAnchors.removeAll { $0.destination == anchor } }
            switch outcome {
            case .inserted:
                if let target = inserter.confirmedAnchor {
                    if let original = destination.target {
                        insertionRebases.inserted(at: original, confirmed: target)
                    }
                    rememberContinuation(record, from: server, at: .field(target))
                }
                message = "Inserted at your cursor"; deliveryStatus = .inserted; status = "Inserted"
            case .copied(let reason):
                transcript = record.finalText; message = reason; deliveryStatus = .copied; status = "Copied"
            case .unconfirmed(let backup):
                transcript = record.finalText
                message = backup ? "Insertion unconfirmed. Copied to clipboard if needed." : "Insertion unconfirmed. Your words are here to copy."
                deliveryStatus = .unconfirmed; status = "Check insertion"
            case .failed(let reason):
                transcript = record.finalText; message = reason; deliveryStatus = .failed; status = "Ready to copy"
            }
        }
        lastTranscript = transcript
        lastAudioSeconds = record.audioSeconds
        lastTranscriptionSeconds = (record.speech?.processingSeconds ?? 0) + (record.proofreading?.processingSeconds ?? 0)
        lastDelivery = message
        lastDeliveryStatus = deliveryStatus
        if sessionID == session { statusMessage = status }
        return DeliveryReceipt(status: deliveryStatus.rawValue, message: message)
    }

    private func rebasedDestination(_ destination: InsertionDestination) -> InsertionDestination {
        guard let target = destination.target else { return destination }
        return .field(insertionRebases.destination(for: target) { TextInserter.unchangedAnchor($0) != nil })
    }

    /// Delivery never overlaps a hold, including a press still inside its acceptance delay.
    private var isHoldingCapture: Bool { isCapturing || hotkey.isHoldInProgress }

    private func waitForCaptureRelease() async {
        while isHoldingCapture, (try? await Task.sleep(for: .milliseconds(50))) != nil {}
    }

    private func stopPendingDictations() {
        for pending in pendingDictations {
            pending.cancel()
            if !pending.sealed {
                removePendingEntry(pending.id)
                Task { try? await pending.client.cancel(pending.id) }
            }
        }
        pendingDictations.removeAll()
        deliveryTail = nil
        insertionRebases.removeAll()
    }

    /// A continuation only means something to the server that produced it.
    private func continuation(for destination: DictationDestination, server: String) -> ContinuationAnchor? {
        let now = ProcessInfo.processInfo.systemUptime
        continuationAnchors.removeAll { now < $0.timestamp || now - $0.timestamp >= 15 * 60 }
        return continuationAnchors.last { $0.destination == destination && $0.server == server }
    }

    private func prepareContinuation(for destination: DictationDestination?) {
        if let destination, let list = continuation(for: destination, server: activeEndpoint)?.continuation.list {
            recordingListHint = list.style == .numbered ? "Continuing at item \(list.nextNumber)" : "Continuing your list"
        } else { recordingListHint = nil }
    }

    private func rememberContinuation(_ record: GenerationRecord, from server: String,
                                      at destination: DictationDestination) {
        continuationAnchors.removeAll { $0.destination == destination }
        // A take that finishes after a switch must not anchor the next take to its old server.
        guard let continuation = record.continuation, server == activeEndpoint else { return }
        continuationAnchors.append(.init(server: server, destination: destination, generationID: record.id,
                                         continuation: continuation, timestamp: ProcessInfo.processInfo.systemUptime))
        continuationAnchors = Array(continuationAnchors.suffix(8))
    }

    private func announceOutcome(_ message: String, _ sound: OutcomeFeedback.Sound?) {
        OutcomeFeedback.announce(message, sound: sound, soundsEnabled: dictationSoundsEnabled)
    }

    private func showError(_ message: String) {
        hudProgressText = nil
        recordingFeedback.reset()
        activity = .failed; errorMessage = message; statusMessage = message
        announceOutcome(message, .failure)
        onHUDVisibility?(true)
        dismissHUDAfter(seconds: 4)
    }

    private func dismissHUDAfter(seconds: Double, clearAttention: Bool = false) {
        hudTask?.cancel()
        hudTask = Task { [weak self] in
            do { try await Task.sleep(for: .seconds(seconds)) } catch { return }
            guard let self, !activity.isBusy else { return }
            if clearAttention { attentionPresented = false }
            // A newer take's result was shown; return the HUD to earlier work.
            if let earlier = pendingDictations.last { showEarlierDictation(earlier); return }
            onHUDVisibility?(false)
            recordingFeedback.reset()
            if (activity == .success || activity == .failed), !attentionPresented { activity = .idle; statusMessage = isServerReady ? "Ready when you are" : serverStatusMessage }
        }
    }

    private func installLifecycleObservers() {
        observers.append(NotificationCenter.default.addObserver(forName: NSApplication.didBecomeActiveNotification, object: nil, queue: .main) {
            [weak self] _ in MainActor.assumeIsolated { self?.refreshPermissions(); self?.refreshServer() }
        })
        workspaceObservers.append(NSWorkspace.shared.notificationCenter.addObserver(forName: NSWorkspace.didWakeNotification, object: nil, queue: .main) {
            [weak self] _ in MainActor.assumeIsolated { self?.refreshServer() }
        })
        for name in [NSWorkspace.willSleepNotification, NSWorkspace.sessionDidResignActiveNotification, NSWorkspace.willPowerOffNotification] {
            workspaceObservers.append(NSWorkspace.shared.notificationCenter.addObserver(forName: name, object: nil, queue: .main) {
                [weak self] _ in MainActor.assumeIsolated { self?.restForSystem() }
            })
        }
        lockObserver = DistributedNotificationCenter.default().addObserver(forName: NSNotification.Name("com.apple.screenIsLocked"), object: nil, queue: .main) {
            [weak self] _ in MainActor.assumeIsolated { self?.restForSystem() }
        }
    }

    /// Joining or leaving a network can change which server is reachable;
    /// check at once instead of waiting for the next periodic check.
    private func startPathMonitor() {
        let monitor = NWPathMonitor()
        monitor.pathUpdateHandler = { [weak self] _ in
            Task { @MainActor [weak self] in self?.refreshServer() }
        }
        monitor.start(queue: DispatchQueue(label: "dev.davis.inlay.network-path"))
        pathMonitor = monitor
    }

    private func restForSystem() {
        stopShortcutCheck()
        // Salvage a live take (best effort before suspend) and leave sealed
        // pendings running: their recovery continues after wake.
        if isCapturing { salvageDictation(reason: "Recording interrupted while your Mac was away.") }
        continuationAnchors.removeAll()
    }
    func refreshPermissions() {
        let current = PermissionSnapshot.capture()
        if current != permissions { permissions = current }
        audioDevices.refresh()
        if permissions.canListenForHotkey {
            isHotkeyActive = hotkey.start()
        } else {
            hotkey.stop()
            isHotkeyActive = false
        }
    }

    func requestMicrophone() {
        if permissions.microphone {
            PermissionManager.openMicrophoneSettings()
            return
        }
        Task {
            _ = await PermissionManager.requestMicrophone()
            refreshPermissions()
        }
    }

    func requestAccessibility() {
        if permissions.accessibility { PermissionManager.openAccessibilitySettings() }
        else { PermissionManager.requestAccessibility() }
        retryPermissions()
    }

    func requestInputMonitoring() {
        if permissions.inputMonitoring { PermissionManager.openInputMonitoringSettings() }
        else { PermissionManager.requestInputMonitoring() }
        retryPermissions()
    }

    func startShortcutCheck() {
        guard !isBusy, !isCheckingShortcut else { return }
        refreshPermissions()
        shortcutCheckStarted = ProcessInfo.processInfo.systemUptime
        shortcutCheckEntries = []
        isCheckingShortcut = true
        hotkey.onDiagnostic = { [weak self] message in self?.appendShortcutCheck(message) }
        hotkey.requireFreshHold()
        appendShortcutCheck("Checking \(shortcut.title) for 60 seconds. The microphone stays off.")
        appendShortcutCheck("Listener: \(isHotkeyActive ? "enabled" : "unavailable"); Accessibility: \(permissions.accessibility); Input Monitoring: \(permissions.inputMonitoring).")
        shortcutCheckTask = Task { [weak self] in
            do { try await Task.sleep(nanoseconds: 60_000_000_000) }
            catch { return }
            guard !Task.isCancelled else { return }
            self?.stopShortcutCheck()
        }
    }

    func stopShortcutCheck() {
        guard isCheckingShortcut else { return }
        // Reset before leaving check mode: a delayed callback must never start
        // the microphone just because this check timed out during a held key.
        hotkey.requireFreshHold()
        appendShortcutCheck("Check ended. No audio was recorded.")
        hotkey.onDiagnostic = nil
        isCheckingShortcut = false
        shortcutCheckTask?.cancel()
        shortcutCheckTask = nil
    }

    private func appendShortcutCheck(_ message: String) {
        guard isCheckingShortcut else { return }
        let elapsed = ProcessInfo.processInfo.systemUptime - shortcutCheckStarted
        let context = NSApp.isActive ? "Inlay" : "background"
        shortcutCheckEntries.append(String(format: "%.2fs", elapsed) + " [\(context)] " + message)
        shortcutCheckEntries = Array(shortcutCheckEntries.suffix(16))
        shortcutCheckText = shortcutCheckEntries.joined(separator: "\n")
    }

    private func startRecordingTimer() {
        stopRecordingTimer()
        let timer = Timer(timeInterval: 0.25, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated {
                guard let self, self.isCapturing else { return }
                let elapsed = ProcessInfo.processInfo.systemUptime - self.recordingStart
                self.recordingFeedback.updateElapsed(elapsed)
                if elapsed >= Double(InlayAPI.maximumRecordingSeconds) { self.finishDictation(atLimit: true) }
            }
        }
        timer.tolerance = 0.025
        recordingTimer = timer
        RunLoop.main.add(timer, forMode: .common)
    }

    private func stopRecordingTimer() {
        recordingTimer?.invalidate()
        recordingTimer = nil
    }

    private func resetLevels() {
        recordingFeedback.clearLevels()
    }

    private func retryPermissions() {
        permissionTask?.cancel()
        permissionTask = Task { [weak self] in
            for _ in 0..<30 {
                do { try await Task.sleep(nanoseconds: 2_000_000_000) }
                catch { return }
                guard let self else { return }
                refreshPermissions()
                if allPermissionsGranted { return }
            }
        }
    }

    private func updateLoginItem() {
        guard !updatingLogin else { return }
        updatingLogin = true
        defer { updatingLogin = false }
        loginItemError = nil
        let status = SMAppService.mainApp.status
        if launchAtLogin, status == .enabled { return }
        if launchAtLogin, status == .requiresApproval {
            loginItemError = "Allow Inlay in System Settings → General → Login Items to finish enabling this preference."
            return
        }
        // A rebuilt accessory app may report .notFound even though login is
        // already off. Do not attempt to unregister an absent service.
        if !launchAtLogin, status != .enabled, status != .requiresApproval { return }
        do {
            if launchAtLogin { try SMAppService.mainApp.register() }
            else { try SMAppService.mainApp.unregister() }
            if launchAtLogin, SMAppService.mainApp.status == .requiresApproval {
                loginItemError = "Allow Inlay in System Settings → General → Login Items to finish enabling this preference."
            }
        } catch {
            // Keep the desired setting consistent between UI and JSON. A denied
            // OS operation must not trigger file rollback/retry feedback loops.
            loginItemError = "Couldn’t update launch at login: \(error.localizedDescription)"
        }
    }


}
