import AppKit
import SwiftUI

// Headless live check: parse one `query state` and read the subscription for a
// few seconds — verifies the live layer without opening a window.
// `RovrGui --live-check`.
if CommandLine.arguments.contains("--live-check") {
    let client = RovrClient(binary: RovrClient.resolveBinary() ?? "")
    if let value = try? client.json(["query", "state"]), let result = value as? [String: Any] {
        let (displays, spaces, windows) = LiveParse.snapshot(from: result)
        print("displays=\(displays.count) spaces=\(spaces.count) windows=\(windows.count)"
            + " onVisibleSpace=\(windows.filter(\.onVisibleSpace).count)")
        for space in spaces {
            print("  space [\(space.id)] display=\(space.displayId) pos=\(space.position)"
                + " focused=\(space.focused) system=\(space.isSystem) label=\(space.label ?? "-")")
        }
    } else {
        print("query state: FAILED")
    }
    let stream = RovrStream(client: client)
    let lock = NSLock()
    var count = 0
    stream.onConnected = { up in print("stream connected=\(up)") }
    stream.onNotification = { notification in
        lock.lock()
        count += 1
        lock.unlock()
        let kind = LiveParse.describe(notification)?.kind ?? (notification["type"] as? String ?? "?")
        print("event: \(kind)")
    }
    stream.start()
    Thread.sleep(forTimeInterval: 4)
    stream.stop()
    print("notifications=\(count)")
    exit(0)
}

// Headless client check: resolves `rovr` and runs one doctor round trip.
// `RovrGui --self-check`.
if CommandLine.arguments.contains("--self-check") {
    let binary = RovrClient.resolveBinary() ?? ""
    print("rovr binary: \(binary.isEmpty ? "NOT FOUND" : binary)")
    let client = RovrClient(binary: binary)
    do {
        let doctor = try client.json(["doctor"]) as? [String: Any] ?? [:]
        print("daemon: ok  generation=\(doctor.int("generation") ?? 0)"
            + " windows=\(doctor.int("windows") ?? 0)"
            + " spaces=\(doctor.int("spaces") ?? 0)"
            + " layout=\(doctor.string("layout") ?? "-")"
            + " config=\(doctor.string("config") ?? "-")")
    } catch {
        print("daemon: error \(error.localizedDescription)")
        exit(1)
    }
    exit(0)
}

/// Programmatic AppKit bootstrap so the GUI runs as a plain SwiftPM executable
/// (no .app bundle required). The full standard menu bar is built by hand and
/// routes commands into the shared `AppState`.
final class AppDelegate: NSObject, NSApplicationDelegate, NSMenuItemValidation {
    private var window: NSWindow?
    private let state = AppState()

    func applicationDidFinishLaunching(_ notification: Notification) {
        buildMainMenu()

        let root = RootView().environmentObject(state)
        let hosting = NSHostingController(rootView: root)

        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 960, height: 680),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered,
            defer: false
        )
        window.title = "Rovr"
        window.minSize = NSSize(width: 780, height: 560)
        window.contentViewController = hosting
        window.setFrameAutosaveName("RovrMainWindow")
        window.toolbarStyle = .unified
        window.collectionBehavior.insert(.fullScreenPrimary)
        window.center()
        window.makeKeyAndOrderFront(nil)
        self.window = window

        NSApp.activate(ignoringOtherApps: true)
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        true
    }

    func applicationWillTerminate(_ notification: Notification) {
        state.stopLive()
    }

    // MARK: - Menu actions

    @objc private func openSettings() {
        state.selectedSection = .settings
    }

    @objc private func toggleSidebar() {
        state.columnVisibility = state.columnVisibility == .detailOnly ? .all : .detailOnly
    }

    @objc private func refreshDiagnostics() {
        state.refresh()
    }

    @objc private func openHelp() {
        if let url = URL(string: "https://github.com/ekasc/rovr") {
            NSWorkspace.shared.open(url)
        }
    }

    // Rule 1.3 — menu items reflect current state.
    func validateMenuItem(_ menuItem: NSMenuItem) -> Bool {
        switch menuItem.action {
        case #selector(refreshDiagnostics):
            return !state.busy
        case #selector(toggleSidebar):
            menuItem.title = state.columnVisibility == .detailOnly ? "Show Sidebar" : "Hide Sidebar"
            return true
        default:
            return true
        }
    }

    // MARK: - Menu bar

    private func buildMainMenu() {
        let mainMenu = NSMenu()

        // App menu
        let appMenuItem = NSMenuItem()
        mainMenu.addItem(appMenuItem)
        let appMenu = NSMenu()
        appMenu.addItem(withTitle: "About Rovr",
                        action: #selector(NSApplication.orderFrontStandardAboutPanel(_:)),
                        keyEquivalent: "")
        appMenu.addItem(.separator())
        let settingsItem = NSMenuItem(title: "Settings…",
                                      action: #selector(openSettings), keyEquivalent: ",")
        settingsItem.target = self
        appMenu.addItem(settingsItem)
        appMenu.addItem(.separator())
        let servicesItem = NSMenuItem(title: "Services", action: nil, keyEquivalent: "")
        let servicesMenu = NSMenu(title: "Services")
        servicesItem.submenu = servicesMenu
        NSApp.servicesMenu = servicesMenu
        appMenu.addItem(servicesItem)
        appMenu.addItem(.separator())
        appMenu.addItem(withTitle: "Hide Rovr",
                        action: #selector(NSApplication.hide(_:)), keyEquivalent: "h")
        let hideOthers = NSMenuItem(title: "Hide Others",
                                    action: #selector(NSApplication.hideOtherApplications(_:)),
                                    keyEquivalent: "h")
        hideOthers.keyEquivalentModifierMask = [.command, .option]
        appMenu.addItem(hideOthers)
        appMenu.addItem(withTitle: "Show All",
                        action: #selector(NSApplication.unhideAllApplications(_:)),
                        keyEquivalent: "")
        appMenu.addItem(.separator())
        appMenu.addItem(withTitle: "Quit Rovr",
                        action: #selector(NSApplication.terminate(_:)), keyEquivalent: "q")
        appMenuItem.submenu = appMenu

        // File menu (no documents; window lifecycle only)
        let fileMenuItem = NSMenuItem()
        mainMenu.addItem(fileMenuItem)
        let fileMenu = NSMenu(title: "File")
        fileMenu.addItem(withTitle: "Close Window",
                         action: #selector(NSWindow.performClose(_:)), keyEquivalent: "w")
        fileMenuItem.submenu = fileMenu

        // Edit menu — standard responder-chain actions drive the focused field.
        let editMenuItem = NSMenuItem()
        mainMenu.addItem(editMenuItem)
        let editMenu = NSMenu(title: "Edit")
        editMenu.addItem(withTitle: "Undo", action: NSSelectorFromString("undo:"), keyEquivalent: "z")
        let redo = NSMenuItem(title: "Redo", action: NSSelectorFromString("redo:"), keyEquivalent: "z")
        redo.keyEquivalentModifierMask = [.command, .shift]
        editMenu.addItem(redo)
        editMenu.addItem(.separator())
        editMenu.addItem(withTitle: "Cut", action: NSSelectorFromString("cut:"), keyEquivalent: "x")
        editMenu.addItem(withTitle: "Copy", action: NSSelectorFromString("copy:"), keyEquivalent: "c")
        editMenu.addItem(withTitle: "Paste", action: NSSelectorFromString("paste:"), keyEquivalent: "v")
        editMenu.addItem(withTitle: "Select All", action: NSSelectorFromString("selectAll:"), keyEquivalent: "a")
        editMenuItem.submenu = editMenu

        // View menu
        let viewMenuItem = NSMenuItem()
        mainMenu.addItem(viewMenuItem)
        let viewMenu = NSMenu(title: "View")
        let reloadItem = NSMenuItem(title: "Reload Diagnostics",
                                    action: #selector(refreshDiagnostics), keyEquivalent: "r")
        reloadItem.target = self
        viewMenu.addItem(reloadItem)
        viewMenu.addItem(.separator())
        let sidebarItem = NSMenuItem(title: "Toggle Sidebar",
                                     action: #selector(toggleSidebar), keyEquivalent: "s")
        sidebarItem.keyEquivalentModifierMask = [.command, .control]
        sidebarItem.target = self
        viewMenu.addItem(sidebarItem)
        viewMenu.addItem(.separator())
        let fullscreenItem = NSMenuItem(title: "Enter Full Screen",
                                        action: #selector(NSWindow.toggleFullScreen(_:)),
                                        keyEquivalent: "f")
        fullscreenItem.keyEquivalentModifierMask = [.command, .control]
        viewMenu.addItem(fullscreenItem)
        viewMenuItem.submenu = viewMenu

        // Window menu
        let windowMenuItem = NSMenuItem()
        mainMenu.addItem(windowMenuItem)
        let windowMenu = NSMenu(title: "Window")
        windowMenu.addItem(withTitle: "Minimize",
                           action: #selector(NSWindow.performMiniaturize(_:)), keyEquivalent: "m")
        windowMenu.addItem(withTitle: "Zoom",
                           action: #selector(NSWindow.performZoom(_:)), keyEquivalent: "")
        windowMenu.addItem(.separator())
        windowMenu.addItem(withTitle: "Bring All to Front",
                           action: #selector(NSApplication.arrangeInFront(_:)), keyEquivalent: "")
        windowMenuItem.submenu = windowMenu

        // Help menu
        let helpMenuItem = NSMenuItem()
        mainMenu.addItem(helpMenuItem)
        let helpMenu = NSMenu(title: "Help")
        let helpItem = NSMenuItem(title: "Rovr Documentation",
                                  action: #selector(openHelp), keyEquivalent: "?")
        helpItem.target = self
        helpMenu.addItem(helpItem)
        helpMenuItem.submenu = helpMenu

        NSApp.mainMenu = mainMenu
        NSApp.windowsMenu = windowMenu
        NSApp.helpMenu = helpMenu
    }
}

let app = NSApplication.shared
app.setActivationPolicy(.regular)
let delegate = AppDelegate()
app.delegate = delegate
app.run()
