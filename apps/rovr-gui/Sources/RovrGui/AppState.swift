import Foundation
import SwiftUI

struct ActionLogEntry: Identifiable {
    let id = UUID()
    let label: String
    let ok: Bool
    let message: String
    let date: Date
}

/// Top-level navigation destinations, shown in the sidebar.
enum SidebarSection: String, CaseIterable, Identifiable, Hashable {
    case live
    case diagnostics
    case features
    case settings

    var id: String { rawValue }

    var title: String {
        switch self {
        case .live: return "Live"
        case .diagnostics: return "Diagnostics"
        case .features: return "Features"
        case .settings: return "Settings"
        }
    }

    var systemImage: String {
        switch self {
        case .live: return "rectangle.3.group"
        case .diagnostics: return "stethoscope"
        case .features: return "slider.horizontal.3"
        case .settings: return "gearshape"
        }
    }
}

/// Observable store shared by all tabs. Owns no domain logic: it caches the
/// daemon's reported state and forwards user intent to the `rovr` CLI.
final class AppState: ObservableObject {
    @Published var selectedSection: SidebarSection? = .live
    @Published var columnVisibility: NavigationSplitViewVisibility = .all

    // Live desktop model, driven by the daemon's `subscribe` stream.
    @Published var liveDisplays: [DisplayBox] = []
    @Published var liveSpaces: [SpaceBox] = []
    @Published var liveWindows: [WindowBox] = []
    @Published var liveEvents: [LiveEvent] = []
    @Published var streamConnected = false
    @Published var healthSamples: [HealthSample] = []
    @Published var lastStateRefresh: Date?

    @Published var doctor: [String: Any]?
    @Published var doctorRaw: String = ""
    @Published var doctorError: String?
    @Published var lastRefresh: Date?

    @Published var events: String = ""
    @Published var eventsError: String?

    @Published var configText: String = ""
    @Published var configPath: String = ""
    @Published var configMessage: String = ""
    @Published var configMessageIsError: Bool = false

    @Published var actionLog: [ActionLogEntry] = []
    @Published var busy: Bool = false

    let client: RovrClient
    private let work = DispatchQueue(label: "rovr.gui.work", qos: .userInitiated)
    private var stream: RovrStream?
    private var stateRefreshWork: DispatchWorkItem?
    private var healthTimer: Timer?

    init() {
        self.client = RovrClient(binary: RovrClient.resolveBinary() ?? "")
        self.configPath = AppState.defaultConfigPath()
    }

    deinit {
        healthTimer?.invalidate()
        stream?.stop()
    }

    static func defaultConfigPath() -> String {
        let home = FileManager.default.homeDirectoryForCurrentUser.path
        return "\(home)/.config/rovr/rovr.toml"
    }

    // MARK: - Derived state

    var capabilities: [String: Bool] {
        guard let caps = doctor?.dict("capabilities") else { return [:] }
        return caps.compactMapValues { $0 as? Bool }
    }

    var health: [String: Any]? { doctor?.dict("health") }
    var accessibility: [String: Any]? { doctor?.dict("accessibility") }
    var sa: [String: Any]? { doctor?.dict("sa") }

    // MARK: - Diagnostics

    func refresh() {
        busy = true
        work.async { [weak self] in
            guard let self else { return }
            let outcome: Result<[String: Any], Error>
            do {
                let value = try self.client.json(["doctor"])
                if let dict = value as? [String: Any] {
                    outcome = .success(dict)
                } else {
                    outcome = .failure(RovrError.malformed("doctor result is not an object"))
                }
            } catch {
                outcome = .failure(error)
            }
            DispatchQueue.main.async {
                self.busy = false
                self.lastRefresh = Date()
                switch outcome {
                case .success(let dict):
                    self.doctor = dict
                    self.doctorRaw = dict.prettyPrinted
                    self.doctorError = nil
                    self.recordHealthSample(dict)
                    if let path = dict.string("config"), !path.isEmpty {
                        self.configPath = path
                    }
                case .failure(let error):
                    self.doctor = nil
                    self.doctorRaw = ""
                    self.doctorError = error.localizedDescription
                }
            }
        }
    }

    func loadEvents() {
        work.async { [weak self] in
            guard let self else { return }
            do {
                let value = try self.client.json(["debug", "events"])
                let text: String
                if let dict = value as? [String: Any] {
                    text = dict.prettyPrinted
                } else if JSONSerialization.isValidJSONObject(value),
                          let data = try? JSONSerialization.data(
                              withJSONObject: value, options: [.prettyPrinted, .sortedKeys]),
                          let rendered = String(data: data, encoding: .utf8) {
                    text = rendered
                } else {
                    text = "\(value)"
                }
                DispatchQueue.main.async {
                    self.events = text
                    self.eventsError = nil
                }
            } catch {
                DispatchQueue.main.async {
                    self.eventsError = error.localizedDescription
                }
            }
        }
    }

    // MARK: - Live desktop

    func startLive() {
        guard stream == nil else { return }
        let stream = RovrStream(client: client)
        stream.onNotification = { [weak self] notification in
            DispatchQueue.main.async { self?.handleNotification(notification) }
        }
        stream.onConnected = { [weak self] up in
            DispatchQueue.main.async { self?.streamConnected = up }
        }
        stream.start()
        self.stream = stream

        if healthTimer == nil {
            let timer = Timer.scheduledTimer(withTimeInterval: 3.0, repeats: true) { [weak self] _ in
                self?.refresh()
            }
            timer.tolerance = 0.5
            healthTimer = timer
        }
        refreshLiveState()
    }

    func stopLive() {
        stateRefreshWork?.cancel()
        stateRefreshWork = nil
        healthTimer?.invalidate()
        healthTimer = nil
        stream?.stop()
        stream = nil
        streamConnected = false
    }

    private func handleNotification(_ notification: [String: Any]) {
        let type = notification.string("type") ?? "unknown"
        if type == "heartbeat" || type == "hello" { return }
        if type == "state_changed" {
            scheduleStateRefresh()
        }
        guard let event = LiveParse.describe(notification) else { return }
        liveEvents.insert(event, at: 0)
        if liveEvents.count > 40 { liveEvents.removeLast(liveEvents.count - 40) }
    }

    /// Coalesce a burst of `state_changed` notifications into one round trip.
    private func scheduleStateRefresh() {
        stateRefreshWork?.cancel()
        let work = DispatchWorkItem { [weak self] in self?.refreshLiveState() }
        stateRefreshWork = work
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.12, execute: work)
    }

    private func refreshLiveState() {
        work.async { [weak self] in
            guard let self else { return }
            guard let value = try? self.client.json(["query", "state"]),
                  let result = value as? [String: Any] else { return }
            let (displays, spaces, windows) = LiveParse.snapshot(from: result)
            DispatchQueue.main.async {
                self.liveDisplays = displays
                self.liveSpaces = spaces
                self.liveWindows = windows
                self.lastStateRefresh = Date()
            }
        }
    }

    private func recordHealthSample(_ doctor: [String: Any]) {
        let health = doctor.dict("health")
        healthSamples.append(
            HealthSample(
                date: Date(),
                heartbeatMs: health?.int("state_loop_heartbeat_age_ms"),
                observationMs: health?.int("last_observation_age_ms"),
                reconcileStreak: health?.int("reconcile_failure_streak") ?? 0
            )
        )
        if healthSamples.count > 60 { healthSamples.removeFirst(healthSamples.count - 60) }
    }

    // MARK: - Feature commands

    /// Run an arbitrary `rovr` command, record it in the activity log, and
    /// refresh diagnostics so the UI reflects the daemon's new state.
    func perform(_ args: [String], label: String? = nil, refreshAfter: Bool = true) {
        let title = label ?? (["rovr"] + args).joined(separator: " ")
        work.async { [weak self] in
            guard let self else { return }
            do {
                let value = try self.client.json(args)
                let message = Self.summarize(value)
                DispatchQueue.main.async {
                    self.appendLog(title: title, ok: true, message: message)
                }
            } catch {
                DispatchQueue.main.async {
                    self.appendLog(title: title, ok: false, message: error.localizedDescription)
                }
            }
            if refreshAfter {
                DispatchQueue.main.async { self.refresh() }
            }
        }
    }

    private static func summarize(_ value: Any) -> String {
        if let dict = value as? [String: Any] {
            if dict.isEmpty { return "ok" }
            return dict.prettyPrinted
        }
        if let array = value as? [Any] {
            return array.isEmpty ? "ok (empty)" : "ok (\(array.count) items)"
        }
        if let flag = value as? Bool { return flag ? "ok" : "ok (false)" }
        return "ok"
    }

    private func appendLog(title: String, ok: Bool, message: String) {
        actionLog.insert(ActionLogEntry(label: title, ok: ok, message: message, date: Date()), at: 0)
        if actionLog.count > 50 { actionLog.removeLast(actionLog.count - 50) }
    }

    // MARK: - Settings

    func loadConfigFromDisk() {
        let path = configPath
        do {
            configText = try String(contentsOfFile: path, encoding: .utf8)
            setConfigMessage("Loaded \(path)", isError: false)
        } catch {
            setConfigMessage("Could not read \(path): \(error.localizedDescription)", isError: true)
        }
    }

    func insertDefaults() {
        work.async { [weak self] in
            guard let self else { return }
            do {
                let result = try self.client.raw(["config", "dump", "--full"])
                DispatchQueue.main.async {
                    self.configText = result.output
                    self.setConfigMessage("Loaded resolved defaults into the editor", isError: false)
                }
            } catch {
                DispatchQueue.main.async {
                    self.setConfigMessage(error.localizedDescription, isError: true)
                }
            }
        }
    }

    func validateConfig() {
        let temp = FileManager.default.temporaryDirectory
            .appendingPathComponent("rovr-gui-check-\(UUID().uuidString).toml")
        do {
            try configText.write(to: temp, atomically: true, encoding: .utf8)
        } catch {
            setConfigMessage("Could not stage config: \(error.localizedDescription)", isError: true)
            return
        }
        work.async { [weak self] in
            guard let self else { return }
            defer { try? FileManager.default.removeItem(at: temp) }
            do {
                _ = try self.client.json(["config", "check", temp.path])
                DispatchQueue.main.async {
                    self.setConfigMessage("Config is valid", isError: false)
                }
            } catch {
                DispatchQueue.main.async {
                    self.setConfigMessage(error.localizedDescription, isError: true)
                }
            }
        }
    }

    /// Persist the editor to disk (with a timestamped backup) and reload the
    /// daemon. Validation happens against the same bytes before writing.
    func saveAndReload() {
        let path = configPath
        let text = configText
        let temp = FileManager.default.temporaryDirectory
            .appendingPathComponent("rovr-gui-save-\(UUID().uuidString).toml")
        do {
            try text.write(to: temp, atomically: true, encoding: .utf8)
        } catch {
            setConfigMessage("Could not stage config: \(error.localizedDescription)", isError: true)
            return
        }
        work.async { [weak self] in
            guard let self else { return }
            defer { try? FileManager.default.removeItem(at: temp) }
            do {
                _ = try self.client.json(["config", "check", temp.path])
            } catch {
                DispatchQueue.main.async {
                    self.setConfigMessage("Not saved — validation failed: \(error.localizedDescription)",
                                          isError: true)
                }
                return
            }
            // Back up the current file before overwriting so a bad edit is recoverable.
            if FileManager.default.fileExists(atPath: path) {
                let stamp = ISO8601DateFormatter().string(from: Date())
                    .replacingOccurrences(of: ":", with: "-")
                try? FileManager.default.copyItem(atPath: path, toPath: "\(path).\(stamp).bak")
            }
            do {
                try text.write(toFile: path, atomically: true, encoding: .utf8)
            } catch {
                DispatchQueue.main.async {
                    self.setConfigMessage("Saved file failed: \(error.localizedDescription)", isError: true)
                }
                return
            }
            do {
                _ = try self.client.json(["config", "reload"])
                DispatchQueue.main.async {
                    self.setConfigMessage("Saved and reloaded \(path)", isError: false)
                    self.appendLog(title: "rovr config reload", ok: true, message: "reloaded")
                }
            } catch {
                DispatchQueue.main.async {
                    self.setConfigMessage("Saved, but reload failed: \(error.localizedDescription)",
                                          isError: true)
                    self.appendLog(title: "rovr config reload", ok: false,
                                   message: error.localizedDescription)
                }
            }
            DispatchQueue.main.async { self.refresh() }
        }
    }

    private func setConfigMessage(_ message: String, isError: Bool) {
        configMessage = message
        configMessageIsError = isError
    }
}
