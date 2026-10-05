import SwiftUI

struct RootView: View {
    @EnvironmentObject var state: AppState

    var body: some View {
        NavigationSplitView(columnVisibility: $state.columnVisibility) {
            List(selection: $state.selectedSection) {
                ForEach(SidebarSection.allCases) { section in
                    Label(section.title, systemImage: section.systemImage)
                        .tag(section)
                }
            }
            .listStyle(.sidebar)
            .navigationSplitViewColumnWidth(min: 180, ideal: 210, max: 280)
        } detail: {
            detail
        }
        .navigationSplitViewStyle(.prominentDetail)
        .frame(minWidth: 780, minHeight: 560)
        .toolbar {
            ToolbarItem(placement: .status) {
                connectionBadge
            }
            ToolbarItem(placement: .primaryAction) {
                Button {
                    state.refresh()
                } label: {
                    Label("Refresh", systemImage: "arrow.clockwise")
                }
                .disabled(state.busy)
                .help("Reload diagnostics (⌘R)")
            }
        }
        .onAppear { state.refresh() }
    }

    @ViewBuilder
    private var detail: some View {
        switch state.selectedSection ?? .live {
        case .live: LiveView()
        case .diagnostics: DiagnosticsView()
        case .features: FeaturesView()
        case .settings: SettingsView()
        }
    }

    private var connectionBadge: some View {
        Group {
            if state.doctorError != nil {
                StatusBadge(text: "Daemon unreachable", tone: .bad)
            } else if state.doctor != nil {
                StatusBadge(text: "Daemon connected", tone: .good)
            } else {
                StatusBadge(text: "Checking…", tone: .neutral)
            }
        }
        .accessibilityLabel(
            state.doctorError != nil ? "Daemon unreachable"
                : state.doctor != nil ? "Daemon connected" : "Checking daemon"
        )
    }
}
