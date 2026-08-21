import AppKit

final class AppDelegate: NSObject, NSApplicationDelegate {
    var statusItem: NSStatusItem!
    var timer: Timer?
    var doctorCache: String = ""

    func applicationDidFinishLaunching(_ notification: Notification) {
        statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        if let button = statusItem.button {
            button.title = "◐ Rovr"
            button.toolTip = "Rovr — window manager"
        }
        rebuildMenu()
        refresh()
        timer = Timer.scheduledTimer(withTimeInterval: 5.0, repeats: true) { [weak self] _ in
            self?.refresh()
        }
    }

    func rebuildMenu() {
        let menu = NSMenu()
        menu.addItem(NSMenuItem(title: "Rovr — Diagnostics", action: nil, keyEquivalent: ""))
        menu.items.last?.isEnabled = false
        menu.addItem(.separator())
        menu.addItem(NSMenuItem(title: "Doctor…", action: #selector(runDoctor), keyEquivalent: "d"))
        menu.addItem(NSMenuItem(title: "State…", action: #selector(showState), keyEquivalent: "s"))
        menu.addItem(NSMenuItem(title: "SA Status…", action: #selector(showSaStatus), keyEquivalent: ""))
        menu.addItem(NSMenuItem(title: "Recent Events…", action: #selector(showEvents), keyEquivalent: "e"))
        menu.addItem(.separator())
        menu.addItem(NSMenuItem(title: "Reload Config", action: #selector(reloadConfig), keyEquivalent: "r"))
        menu.addItem(NSMenuItem(title: "Open Diagnostics Folder", action: #selector(openDiagnostics), keyEquivalent: ""))
        menu.addItem(.separator())
        menu.addItem(NSMenuItem(title: "Quit RovrMenuBar", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "q"))
        statusItem.menu = menu
    }

    @objc func runDoctor() {
        let out = shell("rovr", ["doctor"])
        doctorCache = out
        showAlert(title: "rovr doctor", message: prettyJSON(out) ?? String(out.prefix(6000)))
    }

    @objc func showState() {
        let out = shell("rovr", ["query", "state"])
        showAlert(title: "rovr state", message: prettyJSON(out) ?? String(out.prefix(6000)))
    }

    @objc func showSaStatus() {
        let out = shell("rovr", ["sa", "status"])
        showAlert(title: "rovr sa status", message: String(out.prefix(6000)))
    }

    @objc func showEvents() {
        let out = shell("rovr", ["debug", "events"])
        showAlert(title: "rovr events", message: String(out.prefix(6000)))
    }

    @objc func reloadConfig() {
        let out = shell("rovr", ["config", "reload"])
        showAlert(title: "Reload config", message: out.isEmpty ? "reloaded" : String(out.prefix(4000)))
        refresh()
    }

    @objc func openDiagnostics() {
        let path = ("~/.config/rovr" as NSString).expandingTildeInPath
        NSWorkspace.shared.open(URL(fileURLWithPath: path))
    }

    func refresh() {
        // Use doctor via public IPC (CLI) — no layout logic in Swift
        let out = shell("rovr", ["doctor"])
        doctorCache = out
        // Parse minimal JSON for status without layout logic
        let ok = isDoctorOk(out)
        let sa = saSummary(out)
        let ws = workspaceSummary(out)
        DispatchQueue.main.async { [weak self] in
            guard let self = self else { return }
            let title: String
            if ok {
                title = sa.contains("present: true") ? "◉ Rovr" : "◐ Rovr"
            } else {
                title = "⚠ Rovr"
            }
            self.statusItem.button?.title = title
            var tip = ok ? "Rovr — live" : "Rovr — check doctor"
            if !ws.isEmpty { tip += " • \(ws)" }
            if !sa.isEmpty { tip += " • \(sa)" }
            self.statusItem.button?.toolTip = tip
        }
    }

    func isDoctorOk(_ json: String) -> Bool {
        // doctor returns JSON with capabilities and generation; treat non-empty with windows key as ok
        if json.contains("\"windows\"") || json.contains("\"protocol\"") { return true }
        return json.lowercased().contains("\"ok\"") || json.lowercased().contains("pong")
    }

    func saSummary(_ json: String) -> String {
        if json.contains("\"sa\"") {
            if json.contains("\"present\": true") || json.contains("\"present\":true") {
                return "SA live"
            } else if json.contains("\"present\": false") {
                return "SA missing"
            }
        }
        // fallback to sa status CLI
        let saOut = shell("rovr", ["sa", "status"])
        if saOut.contains("present: true") { return "SA live" }
        if saOut.contains("present: false") { return "SA missing" }
        return ""
    }

    func workspaceSummary(_ json: String) -> String {
        // Try to extract active workspace/layout if present in doctor/state
        // For now just show config layout
        if let data = json.data(using: .utf8),
           let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
           let layout = obj["layout"] as? String {
            return "layout:\(layout)"
        }
        return ""
    }

    func prettyJSON(_ s: String) -> String? {
        guard let data = s.data(using: .utf8),
              let obj = try? JSONSerialization.jsonObject(with: data),
              let pretty = try? JSONSerialization.data(withJSONObject: obj, options: [.prettyPrinted, .sortedKeys]),
              let out = String(data: pretty, encoding: .utf8) else { return nil }
        return out
    }

    func shell(_ launchPath: String, _ args: [String]) -> String {
        let proc = Process()
        proc.executableURL = URL(fileURLWithPath: "/usr/bin/env")
        proc.arguments = [launchPath] + args
        let pipe = Pipe()
        proc.standardOutput = pipe
        proc.standardError = pipe
        try? proc.run()
        proc.waitUntilExit()
        let data = pipe.fileHandleForReading.readDataToEndOfFile()
        return String(data: data, encoding: .utf8) ?? ""
    }

    func showAlert(title: String, message: String) {
        let alert = NSAlert()
        alert.messageText = title
        alert.informativeText = message.isEmpty ? "(no output)" : message
        alert.alertStyle = .informational
        alert.runModal()
    }
}

let delegate = AppDelegate()
let app = NSApplication.shared
app.delegate = delegate
app.setActivationPolicy(.accessory)
app.run()
