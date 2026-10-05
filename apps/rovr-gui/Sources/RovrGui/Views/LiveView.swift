import SwiftUI

struct LiveView: View {
    @EnvironmentObject var state: AppState

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                spacesCard
                healthCard
                eventsCard
            }
            .padding(20)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .onAppear { state.startLive() }
        .onDisappear { state.stopLive() }
    }

    // MARK: - Spaces

    private var spacesCard: some View {
        Card(title: "Spaces") {
            HStack(spacing: 10) {
                StatusBadge(
                    text: state.streamConnected ? "Streaming" : "Reconnecting",
                    tone: state.streamConnected ? .good : .warn
                )
                .accessibilityLabel(state.streamConnected ? "Event stream connected" : "Event stream reconnecting")
                if let refreshed = state.lastStateRefresh {
                    Text("state updated \(refreshed.formatted(date: .omitted, time: .standard))")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                Spacer()
                Text("\(state.liveSpaces.filter { !$0.isSystem }.count) spaces · \(state.liveDisplays.count) displays")
                    .font(.caption.monospacedDigit())
                    .foregroundStyle(.secondary)
            }

            if state.liveDisplays.isEmpty {
                Text("Waiting for the daemon to report the desktop…")
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, minHeight: 200)
            } else {
                VStack(alignment: .leading, spacing: 16) {
                    ForEach(state.liveDisplays) { display in
                        displaySection(display)
                    }
                }
            }

            HStack(spacing: 16) {
                legendSwatch(color: .accentColor, label: "Focused")
                legendSwatch(color: Color.primary.opacity(0.18), label: "Other")
                legendSwatch(color: .secondary, dashed: true, label: "Desired (drift)")
                Spacer()
            }
            .font(.caption)
            .foregroundStyle(.secondary)
        }
    }

    private func displaySection(_ display: DisplayBox) -> some View {
        let list = spaces(for: display)
        let active = list.first(where: \.focused)
        return VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 8) {
                Text(display.isMain ? "Main display" : "Display \(display.id)")
                    .font(.subheadline.weight(.semibold))
                Text("\(Int(display.frame.width))×\(Int(display.frame.height))")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Spacer()
                if let active, let index = list.firstIndex(where: { $0.id == active.id }) {
                    Text("active · \(spaceTitle(active, index: index))")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
            if list.isEmpty {
                Text("No user Spaces observed on this display.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            } else {
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(alignment: .top, spacing: 12) {
                        ForEach(Array(list.enumerated()), id: \.element.id) { index, space in
                            SpacePanel(
                                display: display,
                                title: spaceTitle(space, index: index),
                                windows: windows(in: space, of: display),
                                isActive: space.id == activeSpaceId(for: display),
                                panelHeight: 132
                            )
                        }
                    }
                    .padding(.bottom, 2)
                }
            }
        }
    }

    /// Windows on a Space, plus — on the active Space — any observation-gap
    /// windows (no resolvable Space) so they are never silently dropped.
    private func windows(in space: SpaceBox, of display: DisplayBox) -> [WindowBox] {
        var result = state.liveWindows.filter { $0.spaceId == space.id }
        if space.id == activeSpaceId(for: display) {
            let known = Set(state.liveSpaces.map(\.id))
            result += state.liveWindows.filter { window in
                let unassigned = window.spaceId.map { !known.contains($0) } ?? true
                return unassigned && window.displayId == display.id
            }
        }
        return result
    }

    private func spaces(for display: DisplayBox) -> [SpaceBox] {
        state.liveSpaces.filter { $0.displayId == display.id && !$0.isSystem }
    }

    private func activeSpaceId(for display: DisplayBox) -> UInt64? {
        spaces(for: display).first(where: \.focused)?.id
    }

    private func spaceTitle(_ space: SpaceBox, index: Int) -> String {
        if let label = space.label, !label.isEmpty { return label }
        return "Desktop \(index + 1)"
    }

    private func legendSwatch(color: Color, dashed: Bool = false, label: String) -> some View {
        HStack(spacing: 6) {
            RoundedRectangle(cornerRadius: 2)
                .strokeBorder(style: StrokeStyle(lineWidth: 1.5, dash: dashed ? [3, 2] : []))
                .foregroundStyle(color)
                .frame(width: 14, height: 10)
            Text(label)
        }
        .accessibilityElement(children: .combine)
    }

    // MARK: - Health

    private var healthCard: some View {
        let samples = state.healthSamples
        let heartbeat = samples.compactMap { $0.heartbeatMs.map(Double.init) }
        let observation = samples.compactMap { $0.observationMs.map(Double.init) }
        let streak = samples.map { Double($0.reconcileStreak) }
        return Card(title: "Health") {
            VStack(alignment: .leading, spacing: 12) {
                Sparkline(
                    title: "State-loop heartbeat age",
                    values: heartbeat,
                    valueLabel: ms(samples.last?.heartbeatMs),
                    color: .green
                )
                Sparkline(
                    title: "Last observation age",
                    values: observation,
                    valueLabel: ms(samples.last?.observationMs),
                    color: .blue
                )
                Sparkline(
                    title: "Reconcile failure streak",
                    values: streak,
                    valueLabel: "\(samples.last?.reconcileStreak ?? 0)",
                    color: .orange
                )
            }
        }
    }

    private func ms(_ value: Int?) -> String {
        guard let value else { return "—" }
        return value >= 1000 ? String(format: "%.1f s", Double(value) / 1000) : "\(value) ms"
    }

    // MARK: - Events

    private var eventsCard: some View {
        Card(title: "Live events") {
            if state.liveEvents.isEmpty {
                Text("Waiting for events…")
                    .foregroundStyle(.secondary)
            } else {
                VStack(alignment: .leading, spacing: 6) {
                    ForEach(state.liveEvents.prefix(14)) { event in
                        HStack(spacing: 8) {
                            Circle()
                                .fill(toneColor(event.tone))
                                .frame(width: 6, height: 6)
                                .accessibilityHidden(true)
                            Text(event.kind).fontWeight(.medium)
                            Text(event.detail)
                                .foregroundStyle(.secondary)
                                .lineLimit(1)
                            Spacer(minLength: 8)
                            Text(event.date.formatted(date: .omitted, time: .standard))
                                .font(.caption.monospacedDigit())
                                .foregroundStyle(.tertiary)
                        }
                        .accessibilityElement(children: .combine)
                        .transition(.move(edge: .top).combined(with: .opacity))
                    }
                }
                .animation(.easeOut(duration: 0.2), value: state.liveEvents)
            }
        }
    }

    private func toneColor(_ tone: LiveEvent.Tone) -> Color {
        switch tone {
        case .info: return .secondary
        case .good: return .green
        case .warn: return .orange
        }
    }
}

// MARK: - Space panel

/// One Space of one display, drawn as its own region. Windows are placed
/// inside it, so windows from different Spaces can never overlap — only
/// windows genuinely on the same Space stack.
private struct SpacePanel: View {
    let display: DisplayBox
    let title: String
    let windows: [WindowBox]
    let isActive: Bool
    let panelHeight: CGFloat
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        let aspect = display.frame.width / max(display.frame.height, 1)
        let width = (panelHeight * aspect).rounded()
        let map = MapTransform(
            displays: [display],
            size: CGSize(width: width, height: panelHeight),
            padding: 6
        )
        let animation: Animation? = reduceMotion ? nil : .spring(response: 0.42, dampingFraction: 0.82)

        VStack(spacing: 0) {
            ZStack(alignment: .topLeading) {
                RoundedRectangle(cornerRadius: 8, style: .continuous)
                    .fill(Color.primary.opacity(isActive ? 0.06 : 0.03))
                RoundedRectangle(cornerRadius: 8, style: .continuous)
                    .stroke(
                        isActive ? Color.accentColor : Color(nsColor: .separatorColor),
                        lineWidth: isActive ? 2 : 1
                    )

                ForEach(windows.sorted { ($0.focused ? 1 : 0) < ($1.focused ? 1 : 0) }) { window in
                    WindowCell(window: window, map: map, animation: animation, dimmed: !window.onVisibleSpace)
                }
            }
            .frame(width: width, height: panelHeight)
            .clipShape(RoundedRectangle(cornerRadius: 8, style: .continuous))
            .animation(animation, value: windows)
            .overlay(alignment: .topLeading) {
                Text(title)
                    .font(.caption2.weight(isActive ? .semibold : .regular))
                    .foregroundStyle(isActive ? .primary : .secondary)
                    .padding(5)
            }
            .overlay(alignment: .bottomTrailing) {
                Text("\(windows.count)")
                    .font(.caption2.monospacedDigit())
                    .foregroundStyle(.secondary)
                    .padding(5)
            }
        }
        .frame(width: width)
        .accessibilityElement(children: .contain)
        .accessibilityLabel("\(title)\(isActive ? ", active Space" : ""), \(windows.count) windows")
    }
}

// MARK: - Geometry
/// Fits the union of the given display frames into the available space.
private struct MapTransform {
    let scale: CGFloat
    private let origin: CGPoint
    private let offset: CGPoint

    init(displays: [DisplayBox], size: CGSize, padding: CGFloat = 18) {
        let frames = displays.map(\.frame)
        let minX = frames.map(\.minX).min() ?? 0
        let minY = frames.map(\.minY).min() ?? 0
        let maxX = frames.map(\.maxX).max() ?? 1
        let maxY = frames.map(\.maxY).max() ?? 1
        let unionWidth = max(maxX - minX, 1)
        let unionHeight = max(maxY - minY, 1)
        let usableWidth = max(size.width - padding * 2, 1)
        let usableHeight = max(size.height - padding * 2, 1)
        scale = min(usableWidth / unionWidth, usableHeight / unionHeight)
        origin = CGPoint(x: minX, y: minY)
        offset = CGPoint(
            x: padding + (usableWidth - unionWidth * scale) / 2,
            y: padding + (usableHeight - unionHeight * scale) / 2
        )
    }

    func rect(_ r: CGRect) -> CGRect {
        CGRect(
            x: offset.x + (r.minX - origin.x) * scale,
            y: offset.y + (r.minY - origin.y) * scale,
            width: r.width * scale,
            height: r.height * scale
        )
    }
}

// MARK: - Window cell

private struct WindowCell: View {
    let window: WindowBox
    let map: MapTransform
    let animation: Animation?
    let dimmed: Bool

    var body: some View {
        let rect = map.rect(window.frame)
        ZStack {
            if let desired = window.desiredFrame, desired != window.frame, !dimmed {
                let ghost = map.rect(desired)
                RoundedRectangle(cornerRadius: 3, style: .continuous)
                    .strokeBorder(style: StrokeStyle(lineWidth: 1, dash: [3, 3]))
                    .foregroundStyle(.secondary)
                    .frame(width: max(ghost.width, 2), height: max(ghost.height, 2))
                    .position(x: ghost.midX, y: ghost.midY)
            }

            RoundedRectangle(cornerRadius: 3, style: .continuous)
                .fill(fillColor)
                .overlay(
                    RoundedRectangle(cornerRadius: 3, style: .continuous)
                        .stroke(strokeColor, lineWidth: 1)
                )
                .overlay(alignment: .topLeading) {
                    if rect.width > 58, rect.height > 20 {
                        Text(window.app.isEmpty ? window.title : window.app)
                            .font(.system(size: 9))
                            .lineLimit(1)
                            .padding(.horizontal, 3)
                            .padding(.vertical, 1)
                            .foregroundStyle(labelColor)
                    }
                }
                .frame(width: max(rect.width, 2), height: max(rect.height, 2))
                .position(x: rect.midX, y: rect.midY)
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(
            "\(window.app)\(window.title.isEmpty ? "" : ", \(window.title)")"
                + (window.focused ? ", focused" : "")
                + (dimmed ? ", unplaced" : "")
                + (window.desiredFrame != nil && window.desiredFrame != window.frame ? ", moving" : "")
        )
    }

    private var fillColor: Color {
        if dimmed { return Color.primary.opacity(0.05) }
        return window.focused ? Color.accentColor.opacity(0.85) : Color.primary.opacity(0.14)
    }

    private var strokeColor: Color {
        dimmed ? Color.secondary.opacity(0.35) : Color.primary.opacity(0.18)
    }

    private var labelColor: Color {
        if dimmed { return .secondary }
        return window.focused ? .white : .primary
    }
}

// MARK: - Sparkline

private struct Sparkline: View {
    let title: String
    let values: [Double]
    let valueLabel: String
    let color: Color
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack {
                Text(title)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Spacer()
                Text(valueLabel)
                    .font(.caption.monospacedDigit().weight(.semibold))
            }
            Canvas { context, size in
                guard values.count > 1, let low = values.min(), let high = values.max() else {
                    return
                }
                let span = max(high - low, 0.0001)
                func point(_ index: Int) -> CGPoint {
                    let x = size.width * CGFloat(index) / CGFloat(values.count - 1)
                    let y = size.height - CGFloat((values[index] - low) / span) * (size.height - 4) - 2
                    return CGPoint(x: x, y: y)
                }
                var line = Path()
                line.move(to: point(0))
                for index in 1..<values.count { line.addLine(to: point(index)) }

                var area = line
                area.addLine(to: CGPoint(x: size.width, y: size.height))
                area.addLine(to: CGPoint(x: 0, y: size.height))
                area.closeSubpath()

                context.fill(area, with: .color(color.opacity(0.12)))
                context.stroke(line, with: .color(color), style: StrokeStyle(lineWidth: 1.5, lineJoin: .round))
            }
            .frame(height: 34)
            .animation(reduceMotion ? nil : .easeOut(duration: 0.25), value: values)
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("\(title): \(valueLabel)")
    }
}
