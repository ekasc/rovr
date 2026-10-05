import Foundation

/// Long-lived `rovr subscribe` reader.
///
/// All mutable state lives on one serial queue, so the readability handler,
/// the termination handler, and `stop()` never touch the buffer or process
/// concurrently. Reconnects on its own with a fixed backoff until stopped.
final class RovrStream {
    private let client: RovrClient
    private let queue = DispatchQueue(label: "rovr.gui.stream")
    private var process: Process?
    private var buffer = Data()
    private var stopped = false
    private var connected = false

    /// Called for each parsed notification. Delivered on the stream queue.
    var onNotification: (([String: Any]) -> Void)?
    /// Called when the connection state changes. Delivered on the stream queue.
    var onConnected: ((Bool) -> Void)?

    init(client: RovrClient) {
        self.client = client
    }

    func start() {
        queue.async { [weak self] in
            guard let self else { return }
            self.stopped = false
            self.launch()
        }
    }

    func stop() {
        queue.async { [weak self] in
            guard let self else { return }
            self.stopped = true
            self.teardown()
        }
    }

    // MARK: - Queue-confined

    private func launch() {
        guard !stopped, !client.binary.isEmpty else { return }
        let proc = Process()
        proc.executableURL = URL(fileURLWithPath: client.binary)
        proc.arguments = ["subscribe"]
        let out = Pipe()
        proc.standardOutput = out
        proc.standardError = Pipe()

        out.fileHandleForReading.readabilityHandler = { [weak self] handle in
            let data = handle.availableData
            guard !data.isEmpty else { return }
            self?.queue.async { self?.ingest(data) }
        }
        proc.terminationHandler = { [weak self] _ in
            guard let self else { return }
            self.queue.async {
                out.fileHandleForReading.readabilityHandler = nil
                self.process = nil
                if self.connected {
                    self.connected = false
                    self.onConnected?(false)
                }
                guard !self.stopped else { return }
                self.queue.asyncAfter(deadline: .now() + 2) { self.launch() }
            }
        }

        do {
            try proc.run()
        } catch {
            if connected {
                connected = false
                onConnected?(false)
            }
            guard !stopped else { return }
            queue.asyncAfter(deadline: .now() + 2) { self.launch() }
            return
        }
        process = proc
    }

    private func teardown() {
        process?.terminationHandler = nil
        process?.terminate()
        process = nil
        buffer.removeAll()
        if connected {
            connected = false
            onConnected?(false)
        }
    }

    private func ingest(_ data: Data) {
        buffer.append(data)
        while let newline = buffer.firstIndex(of: 0x0A) {
            let lineData = buffer.subdata(in: buffer.startIndex..<newline)
            buffer.removeSubrange(buffer.startIndex...newline)
            guard let line = String(data: lineData, encoding: .utf8)?
                .trimmingCharacters(in: .whitespaces), !line.isEmpty else { continue }
            guard let object = try? JSONSerialization.jsonObject(with: Data(line.utf8)) as? [String: Any] else {
                continue
            }
            if !connected {
                connected = true
                onConnected?(true)
            }
            onNotification?(object)
        }
    }
}
