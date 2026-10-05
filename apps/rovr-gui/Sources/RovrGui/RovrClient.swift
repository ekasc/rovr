import CoreGraphics
import Foundation

/// Result of running the `rovr` CLI once.
struct CommandResult {
    let stdout: String
    let stderr: String
    let exitCode: Int32

    /// Best-effort human-readable output: stdout when present, else stderr.
    var output: String {
        let out = stdout.trimmingCharacters(in: .whitespacesAndNewlines)
        if !out.isEmpty { return out }
        return stderr.trimmingCharacters(in: .whitespacesAndNewlines)
    }
}

enum RovrError: Error, LocalizedError {
    case binaryNotFound
    case launch(String)
    case timedOut(TimeInterval)
    case malformed(String)
    case daemon(code: String, message: String)

    var errorDescription: String? {
        switch self {
        case .binaryNotFound:
            return "Could not find the `rovr` CLI. Put it on PATH or set ROVR_BIN."
        case .launch(let message):
            return "Failed to launch rovr: \(message)"
        case .timedOut(let seconds):
            return "rovr did not respond within \(Int(seconds))s — is the daemon running?"
        case .malformed(let detail):
            return "Unexpected rovr output: \(detail)"
        case .daemon(let code, let message):
            return message.isEmpty ? code : "\(message) [\(code)]"
        }
    }
}

/// Thin client over the public `rovr` IPC surface.
///
/// The GUI owns no product logic: it shells out to the `rovr` CLI (which is
/// itself a thin IPC client), exactly like `apps/rovr-menu-bar`. All mutations
/// go through the daemon.
final class RovrClient {
    /// Absolute path to the `rovr` executable.
    let binary: String

    init(binary: String) {
        self.binary = binary
    }

    /// Resolve the `rovr` executable the way a user's shell would: an explicit
    /// override, then PATH via a login shell, then common install locations and
    /// the local cargo target dirs.
    static func resolveBinary() -> String? {
        let fm = FileManager.default

        if let override = ProcessInfo.processInfo.environment["ROVR_BIN"],
           !override.isEmpty, fm.isExecutableFile(atPath: override) {
            return override
        }

        // A login shell sees the user's PATH even when the app is launched from
        // Finder (where PATH is minimal).
        if let path = loginShellWhich("rovr") {
            return path
        }

        let home = fm.homeDirectoryForCurrentUser.path
        var candidates = [
            "\(home)/.local/bin/rovr",
            "/usr/local/bin/rovr",
            "/opt/homebrew/bin/rovr",
            "/usr/bin/rovr",
        ]
        // Dev builds: walk up from the working directory looking for target/.
        var dir = URL(fileURLWithPath: fm.currentDirectoryPath)
        for _ in 0..<4 {
            candidates.append(dir.appendingPathComponent("target/release/rovr").path)
            candidates.append(dir.appendingPathComponent("target/debug/rovr").path)
            dir.deleteLastPathComponent()
        }
        return candidates.first { fm.isExecutableFile(atPath: $0) }
    }

    private static func loginShellWhich(_ name: String) -> String? {
        let proc = Process()
        proc.executableURL = URL(fileURLWithPath: "/bin/sh")
        proc.arguments = ["-lc", "command -v \(name)"]
        let pipe = Pipe()
        proc.standardOutput = pipe
        proc.standardError = Pipe()
        do {
            try proc.run()
        } catch {
            return nil
        }
        proc.waitUntilExit()
        guard proc.terminationStatus == 0 else { return nil }
        let data = pipe.fileHandleForReading.readDataToEndOfFile()
        let path = String(data: data, encoding: .utf8)?
            .trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        return path.isEmpty ? nil : path
    }

    /// Run the CLI once with a bounded deadline. Output is captured through
    /// temp files so a chatty child can never deadlock on a full pipe.
    func raw(_ args: [String], timeout: TimeInterval = 10) throws -> CommandResult {
        guard !binary.isEmpty else { throw RovrError.binaryNotFound }

        let tmp = FileManager.default.temporaryDirectory
        let outURL = tmp.appendingPathComponent("rovr-gui-\(UUID().uuidString).out")
        let errURL = tmp.appendingPathComponent("rovr-gui-\(UUID().uuidString).err")
        FileManager.default.createFile(atPath: outURL.path, contents: nil)
        FileManager.default.createFile(atPath: errURL.path, contents: nil)
        defer {
            try? FileManager.default.removeItem(at: outURL)
            try? FileManager.default.removeItem(at: errURL)
        }
        guard let outHandle = try? FileHandle(forWritingTo: outURL),
              let errHandle = try? FileHandle(forWritingTo: errURL) else {
            throw RovrError.launch("cannot open capture files")
        }

        let proc = Process()
        proc.executableURL = URL(fileURLWithPath: binary)
        proc.arguments = args
        proc.standardOutput = outHandle
        proc.standardError = errHandle

        let done = DispatchSemaphore(value: 0)
        proc.terminationHandler = { _ in done.signal() }

        do {
            try proc.run()
        } catch {
            try? outHandle.close()
            try? errHandle.close()
            throw RovrError.launch(error.localizedDescription)
        }

        if done.wait(timeout: .now() + timeout) == .timedOut {
            proc.terminate()
            if done.wait(timeout: .now() + 1) == .timedOut {
                kill(proc.processIdentifier, SIGKILL)
                _ = done.wait(timeout: .now() + 1)
            }
            try? outHandle.close()
            try? errHandle.close()
            throw RovrError.timedOut(timeout)
        }

        try? outHandle.close()
        try? errHandle.close()
        let outData = (try? Data(contentsOf: outURL)) ?? Data()
        let errData = (try? Data(contentsOf: errURL)) ?? Data()
        return CommandResult(
            stdout: String(data: outData, encoding: .utf8) ?? "",
            stderr: String(data: errData, encoding: .utf8) ?? "",
            exitCode: proc.terminationStatus
        )
    }

    /// Run a JSON command and unwrap the typed IPC envelope, mapping a daemon
    /// error outcome to a thrown `RovrError.daemon`.
    func json(_ args: [String], timeout: TimeInterval = 10) throws -> Any {
        let result = try raw(args, timeout: timeout)
        let text = result.stdout.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty, let data = text.data(using: .utf8),
              let envelope = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else {
            let detail = result.stderr.trimmingCharacters(in: .whitespacesAndNewlines)
            throw RovrError.malformed(detail.isEmpty ? "no JSON output" : detail)
        }
        if let status = envelope["status"] as? String, status == "ok" {
            return envelope["result"] ?? [:] as Any
        }
        let error = envelope["error"] as? [String: Any]
        throw RovrError.daemon(
            code: error?["code"] as? String ?? "ERROR",
            message: error?["message"] as? String ?? "unknown daemon error"
        )
    }
}

extension Dictionary where Key == String, Value == Any {
    func string(_ key: String) -> String? { self[key] as? String }
    func int(_ key: String) -> Int? { (self[key] as? NSNumber)?.intValue }
    func bool(_ key: String) -> Bool? { self[key] as? Bool }
    func dict(_ key: String) -> [String: Any]? { self[key] as? [String: Any] }

    /// Decode a nested `{x,y,width,height}` rect.
    func rect(_ key: String) -> CGRect? {
        guard let d = self[key] as? [String: Any] else { return nil }
        let value = { (name: String) in CGFloat((d[name] as? NSNumber)?.doubleValue ?? 0) }
        return CGRect(x: value("x"), y: value("y"), width: value("width"), height: value("height"))
    }

    /// Stable, human-readable rendering of a JSON value for detail panes.
    var prettyPrinted: String {
        guard JSONSerialization.isValidJSONObject(self),
              let data = try? JSONSerialization.data(
                  withJSONObject: self, options: [.prettyPrinted, .sortedKeys]),
              let text = String(data: data, encoding: .utf8) else { return "" }
        return text
    }
}
