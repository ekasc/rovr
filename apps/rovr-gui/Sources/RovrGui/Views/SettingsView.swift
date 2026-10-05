import SwiftUI

struct SettingsView: View {
    @EnvironmentObject var state: AppState

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            header
            editor
            footer
        }
        .padding(20)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .leading)
        .onAppear {
            if state.configText.isEmpty {
                state.loadConfigFromDisk()
            }
        }
    }

    private var header: some View {
        Card(title: "Configuration") {
            KeyValueRow(key: "File", value: state.configPath)
            if let doctor = state.doctor {
                KeyValueRow(key: "Active layout", value: doctor.string("layout") ?? "—")
                KeyValueRow(key: "Gap", value: "\(doctor.int("gap") ?? 0) px")
                KeyValueRow(key: "Reconcile interval",
                            value: "\(doctor.int("reconcile_interval_ms") ?? 0) ms")
            }
            HStack {
                Button("Reload from disk") { state.loadConfigFromDisk() }
                Button("Insert resolved defaults") { state.insertDefaults() }
                Button("Reveal in Finder") {
                    NSWorkspace.shared.activateFileViewerSelecting(
                        [URL(fileURLWithPath: state.configPath)])
                }
            }
        }
    }

    private var editor: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("TOML")
                .font(.headline)
            TextEditor(text: $state.configText)
                .font(.system(.body, design: .monospaced))
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .overlay(
                    RoundedRectangle(cornerRadius: 6)
                        .stroke(Color(nsColor: .separatorColor), lineWidth: 1)
                )
                .accessibilityLabel("Rovr configuration TOML")
        }
    }

    private var footer: some View {
        HStack(spacing: 12) {
            Button("Validate") { state.validateConfig() }
            Button("Save & Reload") { state.saveAndReload() }
                .keyboardShortcut("s", modifiers: [.command])
            if !state.configMessage.isEmpty {
                Text(state.configMessage)
                    .font(.callout)
                    .foregroundStyle(state.configMessageIsError ? .red : .secondary)
                    .textSelection(.enabled)
                    .lineLimit(2)
            }
            Spacer()
        }
    }
}
