import AppKit
import SwiftUI

/// Bordered section with a title, used across all tabs.
struct Card<Content: View>: View {
    let title: String
    @ViewBuilder var content: Content

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(title)
                .font(.headline)
            content
        }
        .padding(14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: 10, style: .continuous)
                .fill(Color(nsColor: .controlBackgroundColor))
        )
        .overlay(
            RoundedRectangle(cornerRadius: 10, style: .continuous)
                .stroke(Color(nsColor: .separatorColor), lineWidth: 1)
        )
    }
}

/// A key/value line. Values are right-aligned and selectable.
struct KeyValueRow: View {
    let key: String
    let value: String
    var emphasis: Bool = false

    var body: some View {
        HStack(alignment: .firstTextBaseline) {
            Text(key)
                .foregroundStyle(.secondary)
            Spacer(minLength: 12)
            Text(value)
                .fontWeight(emphasis ? .semibold : .regular)
                .textSelection(.enabled)
                .multilineTextAlignment(.trailing)
        }
    }
}

/// Textual status badge. Never relies on colour alone — the word carries the
/// meaning.
struct StatusBadge: View {
    let text: String
    let tone: Tone

    enum Tone { case good, bad, warn, neutral }

    private var color: Color {
        switch tone {
        case .good: return .green
        case .bad: return .red
        case .warn: return .orange
        case .neutral: return .secondary
        }
    }

    var body: some View {
        Text(text)
            .font(.caption.weight(.semibold))
            .padding(.horizontal, 8)
            .padding(.vertical, 3)
            .background(color.opacity(0.15), in: Capsule())
            .overlay(Capsule().stroke(color.opacity(0.4), lineWidth: 1))
            .foregroundStyle(color)
    }
}

/// Capability name → availability, rendered as an accessible list.
struct CapabilityList: View {
    let capabilities: [String: Bool]

    private var sortedKeys: [String] {
        capabilities.keys.sorted()
    }

    var body: some View {
        if capabilities.isEmpty {
            Text("No capability data.")
                .foregroundStyle(.secondary)
        } else {
            LazyVGrid(columns: [GridItem(.adaptive(minimum: 220), spacing: 8)],
                      alignment: .leading, spacing: 8) {
                ForEach(sortedKeys, id: \.self) { key in
                    let available = capabilities[key] ?? false
                    HStack(spacing: 8) {
                        Image(systemName: available ? "checkmark.circle.fill" : "slash.circle")
                            .foregroundStyle(available ? .green : .secondary)
                            .accessibilityHidden(true)
                        Text(key.replacingOccurrences(of: "_", with: " "))
                            .textSelection(.enabled)
                        Spacer(minLength: 4)
                        Text(available ? "Available" : "Unsupported")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                    .accessibilityElement(children: .combine)
                    .contextMenu {
                        Button("Copy Capability Name") {
                            NSPasteboard.general.clearContents()
                            NSPasteboard.general.setString(key, forType: .string)
                        }
                    }
                }
            }
        }
    }
}

/// Inline activity log shared by the feature controls.
struct ActionLogView: View {
    let entries: [ActionLogEntry]

    var body: some View {
        if entries.isEmpty {
            Text("No actions yet.")
                .foregroundStyle(.secondary)
        } else {
            VStack(alignment: .leading, spacing: 6) {
                ForEach(entries) { entry in
                    HStack(alignment: .top, spacing: 8) {
                        Image(systemName: entry.ok ? "checkmark.circle.fill" : "xmark.octagon.fill")
                            .foregroundStyle(entry.ok ? .green : .red)
                            .accessibilityHidden(true)
                        VStack(alignment: .leading, spacing: 2) {
                            Text(entry.label).fontWeight(.medium)
                            Text(entry.message)
                                .font(.caption)
                                .foregroundStyle(.secondary)
                                .lineLimit(4)
                                .textSelection(.enabled)
                        }
                        Spacer(minLength: 0)
                    }
                    .accessibilityElement(children: .combine)
                    .contextMenu {
                        Button("Copy") {
                            let text = "\(entry.label)\n\(entry.message)"
                            NSPasteboard.general.clearContents()
                            NSPasteboard.general.setString(text, forType: .string)
                        }
                    }
                }
            }
        }
    }
}

/// A labelled single-line text field with an explicit accessibility label.
struct LabeledField: View {
    let label: String
    let prompt: String
    @Binding var text: String
    var width: CGFloat = 90

    var body: some View {
        HStack(spacing: 6) {
            Text(label)
                .foregroundStyle(.secondary)
            TextField(prompt, text: $text)
                .textFieldStyle(.roundedBorder)
                .frame(width: width)
                .accessibilityLabel(label)
        }
    }
}
