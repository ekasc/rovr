import AppKit

final class AppDelegate: NSObject, NSApplicationDelegate {
    var statusItem: NSStatusItem!
    var timer: Timer?

    func applicationDidFinishLaunching(_ notification: Notification) {
        statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.squareLength)
        if let button = statusItem.button {
            button.title = "◐"
            button.toolTip = "Rovr — window manager"
        }
        let menu = NSMenu()
        menu.addItem(NSMenuItem(title: "Doctor…", action: #selector(runDoctor), keyEquivalent: "d"))
        menu.addItem(NSMenuItem(title: "Events…", action: #selector(showEvents), keyEquivalent: "e"))
        menu.addItem(.separator())
        menu.addItem(NSMenuItem(title: "Quit", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "q"))
        statusItem.menu = menu
        refresh()
        timer = Timer.scheduledTimer(withTimeInterval: 5.0, repeats: true) { [weak self] _ in
            self?.refresh()
        }
    }

    @objc func runDoctor() {
        let out = shell("rovr", ["doctor"])
        showAlert(title: "rovr doctor", message: out)
    }

    @objc func showEvents() {
        let out = shell("rovr", ["debug", "events"])
        showAlert(title: "rovr events", message: String(out.prefix(4000)))
    }

    func refresh() {
        let out = shell("rovr", ["doctor"])
        let ok = out.contains("\"windows\"") || out.lowercased().contains("ok")
        DispatchQueue.main.async { [weak self] in
            self?.statusItem.button?.title = ok ? "◉" : "◐"
            self?.statusItem.button?.toolTip = ok ? "Rovr — live" : "Rovr — check doctor"
        }
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
        alert.runModal()
    }
}

let delegate = AppDelegate()
let app = NSApplication.shared
app.delegate = delegate
app.setActivationPolicy(.accessory)
app.run()
