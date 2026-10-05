import SwiftUI

struct DiagnosticsView: View {
    @EnvironmentObject var state: AppState

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                connectionCard
                if state.doctor != nil {
                    healthCard
                    accessibilityCard
                    capabilitiesCard
                    scriptingAdditionCard
                    eventsCard
                }
            }
            .padding(20)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    private var connectionCard: some View {
        Card(title: "Daemon") {
            HStack(spacing: 10) {
                if state.doctorError != nil {
                    StatusBadge(text: "Unreachable", tone: .bad)
                } else if state.doctor != nil {
                    StatusBadge(text: "Connected", tone: .good)
                } else {
                    StatusBadge(text: "Checking", tone: .neutral)
                }
                if let refreshed = state.lastRefresh {
                    Text("Updated \(refreshed.formatted(date: .omitted, time: .standard))")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                Spacer()
                Text(state.client.binary.isEmpty ? "rovr CLI not found" : state.client.binary)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .textSelection(.enabled)
            }

            if let error = state.doctorError {
                Text(error)
                    .foregroundStyle(.red)
                    .textSelection(.enabled)
            }

            if let doctor = state.doctor {
                Divider()
                KeyValueRow(key: "Protocol", value: "\(doctor.int("protocol") ?? 0)")
                KeyValueRow(key: "Generation", value: "\(doctor.int("generation") ?? 0)")
                KeyValueRow(key: "Windows", value: "\(doctor.int("windows") ?? 0)")
                KeyValueRow(key: "Spaces", value: "\(doctor.int("spaces") ?? 0)")
                KeyValueRow(key: "Displays", value: "\(doctor.int("displays") ?? 0)")
                KeyValueRow(key: "Layout", value: doctor.string("layout") ?? "—")
                KeyValueRow(key: "Gap", value: "\(doctor.int("gap") ?? 0) px")
                KeyValueRow(key: "Reconcile interval",
                            value: "\(doctor.int("reconcile_interval_ms") ?? 0) ms")
                KeyValueRow(key: "Config", value: doctor.string("config") ?? "—")
            }
        }
    }

    private var healthCard: some View {
        Card(title: "Health") {
            if let health = state.health {
                KeyValueRow(key: "State-loop heartbeat age",
                            value: ms(health.int("state_loop_heartbeat_age_ms")))
                KeyValueRow(key: "Last observation age",
                            value: ms(health.int("last_observation_age_ms")))
                KeyValueRow(key: "Reconcile failure streak",
                            value: "\(health.int("reconcile_failure_streak") ?? 0)")
            }
            if let wedged = state.doctor?.int("snapshot_wedged_ms") {
                KeyValueRow(key: "Observation wedged", value: ms(wedged))
            }
        }
    }

    private var accessibilityCard: some View {
        Card(title: "Accessibility") {
            if let accessibility = state.accessibility {
                let available = accessibility.bool("control_available") ?? false
                HStack {
                    Text("Window control")
                    Spacer()
                    StatusBadge(text: available ? "Available" : "Degraded",
                                tone: available ? .good : .warn)
                }
                if let message = accessibility.string("degraded_message"), !available {
                    Text(message)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .textSelection(.enabled)
                }
            } else {
                Text("No accessibility data.").foregroundStyle(.secondary)
            }
        }
    }

    private var capabilitiesCard: some View {
        Card(title: "Capabilities") {
            CapabilityList(capabilities: state.capabilities)
        }
    }

    private var scriptingAdditionCard: some View {
        Card(title: "Scripting Addition") {
            if let sa = state.sa {
                if sa.bool("available") == false {
                    Text("Not available: \(sa.string("reason") ?? "unknown")")
                        .foregroundStyle(.secondary)
                } else {
                    let present = sa.bool("present") ?? false
                    let compatible = sa.bool("compatible") ?? false
                    HStack {
                        Text("Payload")
                        Spacer()
                        if present && compatible {
                            StatusBadge(text: "Injected", tone: .good)
                        } else if present {
                            StatusBadge(text: "Incompatible", tone: .warn)
                        } else {
                            StatusBadge(text: "Not installed", tone: .neutral)
                        }
                    }
                    KeyValueRow(key: "Socket", value: sa.string("socket") ?? "—")
                    KeyValueRow(key: "Version", value: sa.string("version") ?? "—")
                    if let attribs = sa.int("attribs") {
                        KeyValueRow(key: "Attribs", value: String(format: "0x%08x", attribs))
                    }
                    KeyValueRow(key: "Expected prefix", value: sa.string("expected_prefix") ?? "—")

                    if let reinject = state.doctor?.dict("sa_reinject") {
                        Divider()
                        Text("Reinjection").font(.subheadline.weight(.semibold))
                        KeyValueRow(key: "Phase", value: reinject.string("phase") ?? "—")
                        KeyValueRow(key: "Dock PID", value: "\(reinject.int("dock_pid") ?? 0)")
                        KeyValueRow(key: "Attempts this generation",
                                    value: "\(reinject.int("attempts_this_generation") ?? 0)")
                        if let error = reinject.string("last_error"), !error.isEmpty {
                            KeyValueRow(key: "Last error", value: error)
                        }
                    }
                }
            } else {
                Text("No scripting-addition data (non-macOS or older daemon).")
                    .foregroundStyle(.secondary)
            }
        }
    }

    private var eventsCard: some View {
        Card(title: "Recent events") {
            HStack {
                Button("Load events") { state.loadEvents() }
                .help("Run rovr debug events")
                Spacer()
            }
            if let error = state.eventsError {
                Text(error).foregroundStyle(.red)
            }
            if !state.events.isEmpty {
                ScrollView {
                    Text(state.events)
                        .font(.system(.caption, design: .monospaced))
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
                .frame(height: 220)
                .background(Color(nsColor: .textBackgroundColor), in: RoundedRectangle(cornerRadius: 6))
            } else if state.eventsError == nil {
                Text("Not loaded.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
    }

    private func ms(_ value: Int?) -> String {
        guard let value else { return "—" }
        if value >= 1000 {
            return String(format: "%.1f s", Double(value) / 1000.0)
        }
        return "\(value) ms"
    }
}
