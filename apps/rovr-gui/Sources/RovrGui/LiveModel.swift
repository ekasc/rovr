import CoreGraphics
import Foundation

/// One physical display in the live map.
struct DisplayBox: Identifiable, Equatable {
    let id: UInt32
    let frame: CGRect
    let isMain: Bool
    let focused: Bool
}

/// One macOS Space on a display. `position` is its 0-based order on that display.
struct SpaceBox: Identifiable, Equatable {
    let id: UInt64
    let displayId: UInt32
    let position: UInt32
    let focused: Bool
    let label: String?
    let isSystem: Bool
}

/// One observed window in the live map, with the layout the daemon wants.
struct WindowBox: Identifiable, Equatable {
    let id: UInt32
    let frame: CGRect
    let desiredFrame: CGRect?
    let displayId: UInt32?
    let spaceId: UInt64?
    let focused: Bool
    let app: String
    let title: String
    /// True when the window lives on its display's focused Space — i.e. the
    /// desktop currently on screen, not another Space at the same coordinates.
    let onVisibleSpace: Bool
}

/// A point-in-time health reading, sampled on each doctor refresh.
struct HealthSample: Identifiable {
    let id = UUID()
    let date: Date
    let heartbeatMs: Int?
    let observationMs: Int?
    let reconcileStreak: Int
}

/// One entry in the live event ticker.
struct LiveEvent: Identifiable, Equatable {
    enum Tone: Equatable { case info, good, warn }
    let id = UUID()
    let date: Date
    let kind: String
    let detail: String
    let tone: Tone
}

enum LiveParse {
    /// Extract displays and windows from a `query state` result.
    ///
    /// Windows are kept when they have a real frame that intersects a display,
    /// regardless of whether the daemon resolved a Space for them. Windows on
    /// their display's focused Space are marked `onVisibleSpace`; windows on
    /// other Spaces share coordinates with the focused desktop and are only
    /// shown when the caller asks for all Spaces.
    static func snapshot(from result: [String: Any]) -> (displays: [DisplayBox], spaces: [SpaceBox], windows: [WindowBox]) {
        let observed = result.dict("observed") ?? [:]
        let desiredWindows = (result.dict("desired") ?? [:]).dict("windows") ?? [:]

        var displays: [DisplayBox] = []
        for value in (observed.dict("displays") ?? [:]).values {
            guard let raw = value as? [String: Any], let frame = raw.rect("frame") else { continue }
            displays.append(
                DisplayBox(
                    id: UInt32(raw.int("id") ?? 0),
                    frame: frame,
                    isMain: raw.bool("is_main") ?? false,
                    focused: raw.bool("focused") ?? false
                )
            )
        }
        displays.sort { $0.id < $1.id }

        var spaces: [SpaceBox] = []
        var focusedSpaces = Set<UInt64>()
        for value in (observed.dict("spaces") ?? [:]).values {
            guard let raw = value as? [String: Any],
                  let id = (raw["id"] as? NSNumber)?.uint64Value else { continue }
            let focused = raw.bool("focused") ?? false
            if focused { focusedSpaces.insert(id) }
            spaces.append(
                SpaceBox(
                    id: id,
                    displayId: (raw["display_id"] as? NSNumber)?.uint32Value ?? 0,
                    position: UInt32(raw.int("position") ?? 0),
                    focused: focused,
                    label: raw.string("label"),
                    isSystem: raw.bool("is_system") ?? false
                )
            )
        }
        spaces.sort { ($0.displayId, $0.position) < ($1.displayId, $1.position) }

        let union = unionRect(displays)
        var windows: [WindowBox] = []
        for (key, value) in (observed.dict("windows") ?? [:]) {
            guard let raw = value as? [String: Any], let frame = raw.rect("frame") else { continue }
            guard frame.width > 1, frame.height > 1 else { continue }
            guard union?.intersects(frame) ?? false else { continue }
            let desired = desiredWindows[key] as? [String: Any]
            let spaceId = (raw["space_id"] as? NSNumber)?.uint64Value
            windows.append(
                WindowBox(
                    id: UInt32(key) ?? UInt32(raw.int("id") ?? 0),
                    frame: frame,
                    desiredFrame: desired?.rect("frame"),
                    displayId: (raw["display_id"] as? NSNumber)?.uint32Value,
                    spaceId: spaceId,
                    focused: raw.bool("focused") ?? false,
                    app: raw.string("app") ?? "",
                    title: raw.string("title") ?? "",
                    onVisibleSpace: spaceId.map(focusedSpaces.contains) ?? false
                )
            )
        }
        return (displays, spaces, windows)
    }

    private static func unionRect(_ displays: [DisplayBox]) -> CGRect? {
        guard var union = displays.first?.frame else { return nil }
        for display in displays.dropFirst() {
            union = union.union(display.frame)
        }
        return union
    }

    /// Human-readable summary of one subscription notification.
    static func describe(_ notification: [String: Any]) -> LiveEvent? {
        let type = notification.string("type") ?? "unknown"
        let now = Date()
        switch type {
        case "state_changed":
            return LiveEvent(date: now, kind: "State changed", detail: "re-observed desktop", tone: .info)
        case "layout_changed":
            let space = notification.int("space") ?? 0
            let horizontal = notification.bool("horizontal") ?? false
            return LiveEvent(date: now, kind: "Layout changed", detail: "space \(space) · \(horizontal ? "horizontal" : "vertical")", tone: .info)
        case "scratchpad_toggled":
            return LiveEvent(date: now, kind: "Scratchpad", detail: "\(notification.string("name") ?? "?") \(notification.bool("open") == true ? "opened" : "closed")", tone: .info)
        case "config_reloaded":
            return LiveEvent(date: now, kind: "Config reloaded", detail: "hot reload applied", tone: .good)
        default:
            return nil
        }
    }
}
