import SwiftUI

struct FeaturesView: View {
    @EnvironmentObject var state: AppState

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                Card(title: "Capability status") {
                    Text("Controls below issue commands through the daemon. Features that macOS or the scripting addition does not report as available may fail.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    CapabilityList(capabilities: state.capabilities)
                }

                LayoutControls()
                SpaceControls()
                WorkspaceControls()
                ScratchpadControls()
                WindowControls()

                Card(title: "Activity") {
                    ActionLogView(entries: state.actionLog)
                }
            }
            .padding(20)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}

// MARK: - Layout

private struct LayoutControls: View {
    @EnvironmentObject var state: AppState
    @State private var ratio = "0.5"
    @State private var space = ""

    var body: some View {
        Card(title: "Layout") {
            HStack {
                Button("Rotate") { state.perform(["layout", "rotate"]) }
                Button("Mirror") { state.perform(["layout", "mirror"]) }
                Button("Balance") { state.perform(["layout", "balance"]) }
            }
            .help("Applies to the focused space")

            HStack {
                Button("Set ratio") {
                    var args = ["layout", "set-ratio", ratio]
                    if !space.isEmpty { args += ["--space", space] }
                    state.perform(args)
                }
                LabeledField(label: "ratio", prompt: "0.1–0.9", text: $ratio, width: 70)
                LabeledField(label: "space (optional)", prompt: "focused", text: $space)
            }
        }
    }
}

// MARK: - Spaces

private struct SpaceControls: View {
    @EnvironmentObject var state: AppState
    @State private var spaceId = ""
    @State private var afterId = ""
    @State private var anchorId = ""

    var body: some View {
        Card(title: "Spaces") {
            HStack {
                Button("Next") { state.perform(["space", "next"]) }
                Button("Previous") { state.perform(["space", "prev"]) }
                Button("Focus recent") { state.perform(["space", "focus-recent"]) }
                Button("Toggle insets") { state.perform(["space", "toggle-insets"]) }
            }

            HStack {
                Button("Focus") {
                    state.perform(["space", "focus", spaceId])
                }
                .disabled(spaceId.isEmpty)
                LabeledField(label: "space id", prompt: "e.g. 101", text: $spaceId)

                Button("Destroy") {
                    state.perform(["space", "destroy", spaceId])
                }
                .disabled(spaceId.isEmpty)
            }

            HStack {
                Button("Move after") {
                    state.perform(["space", "move", spaceId, afterId])
                }
                .disabled(spaceId.isEmpty || afterId.isEmpty)
                LabeledField(label: "space id", prompt: "e.g. 101", text: $spaceId)
                LabeledField(label: "after", prompt: "e.g. 100", text: $afterId)
            }

            HStack {
                Button("Create") {
                    if anchorId.isEmpty {
                        state.perform(["space", "create"])
                    } else {
                        state.perform(["space", "create", anchorId])
                    }
                }
                LabeledField(label: "anchor (optional)", prompt: "focused", text: $anchorId)
            }
        }
    }
}

// MARK: - Workspaces

private struct WorkspaceControls: View {
    @EnvironmentObject var state: AppState
    @State private var name = ""

    var body: some View {
        Card(title: "Workspaces") {
            HStack {
                Button("Focus") { state.perform(["workspace", "focus", name]) }
                    .disabled(name.isEmpty)
                Button("Move focused window here") {
                    state.perform(["window", "move-to-workspace", name])
                }
                .disabled(name.isEmpty)
                LabeledField(label: "name", prompt: "e.g. code", text: $name, width: 120)
            }
        }
    }
}

// MARK: - Scratchpads

private struct ScratchpadControls: View {
    @EnvironmentObject var state: AppState
    @State private var name = ""

    var body: some View {
        Card(title: "Scratchpads") {
            HStack {
                Button("Toggle") { state.perform(["scratchpad", "toggle", name]) }
                    .disabled(name.isEmpty)
                LabeledField(label: "name", prompt: "e.g. term", text: $name, width: 120)
            }
        }
    }
}

// MARK: - Windows

private struct WindowControls: View {
    @EnvironmentObject var state: AppState
    @State private var windowId = ""
    @State private var spaceId = ""
    @State private var workspace = ""
    @State private var direction = "east"
    @State private var edge = "east"
    @State private var delta = "20"
    @State private var opacity = "0.9"

    private let directions = ["north", "south", "east", "west"]

    var body: some View {
        Card(title: "Windows") {
            Text("Actions without an id apply to the focused window.")
                .font(.caption)
                .foregroundStyle(.secondary)

            HStack {
                Button("Close") { state.perform(["window", "close"]) }
                Button("Toggle fullscreen") { state.perform(["window", "toggle-fullscreen"]) }
                Button("Toggle float") { state.perform(["window", "toggle-float"]) }
            }

            HStack {
                Button("Focus") { state.perform(focusArgs) }
                    .disabled(windowId.isEmpty)
                Button("Move to space") {
                    state.perform(["window", "move-to-space", windowId, spaceId])
                }
                .disabled(windowId.isEmpty || spaceId.isEmpty)
                LabeledField(label: "window id", prompt: "e.g. 482", text: $windowId)
                LabeledField(label: "space id", prompt: "e.g. 101", text: $spaceId)
            }

            HStack {
                Button("Move to workspace") {
                    var args = ["window", "move-to-workspace", workspace]
                    if !windowId.isEmpty { args.append(windowId) }
                    state.perform(args)
                }
                .disabled(workspace.isEmpty)
                LabeledField(label: "workspace", prompt: "e.g. chat", text: $workspace, width: 110)
            }

            HStack {
                Button("Focus direction") {
                    var args = ["window", "focus-direction", direction]
                    if !windowId.isEmpty { args.append(windowId) }
                    state.perform(args)
                }
                Button("Swap direction") {
                    var args = ["window", "swap-dir", direction]
                    if !windowId.isEmpty { args += ["--window", windowId] }
                    state.perform(args)
                }
                Button("Warp direction") {
                    var args = ["window", "warp-dir", direction]
                    if !windowId.isEmpty { args += ["--window", windowId] }
                    state.perform(args)
                }
                Picker("direction", selection: $direction) {
                    ForEach(directions, id: \.self) { Text($0).tag($0) }
                }
                .frame(width: 130)
            }

            HStack {
                Button("Resize edge") {
                    var args = ["window", "resize", edge, delta]
                    if !windowId.isEmpty { args += ["--window", windowId] }
                    state.perform(args)
                }
                Picker("edge", selection: $edge) {
                    ForEach(directions, id: \.self) { Text($0).tag($0) }
                }
                .frame(width: 120)
                LabeledField(label: "delta", prompt: "±points", text: $delta, width: 70)
            }

            HStack {
                Button("Set opacity") {
                    state.perform(["window", "set-opacity", windowId, opacity, "0"])
                }
                .disabled(windowId.isEmpty)
                LabeledField(label: "window id", prompt: "e.g. 482", text: $windowId)
                LabeledField(label: "opacity", prompt: "0.0–1.0", text: $opacity, width: 70)
            }
        }
    }

    private var focusArgs: [String] {
        ["window", "focus", windowId]
    }
}
