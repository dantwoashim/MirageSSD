import AppKit
import Foundation
import Security

struct Failure: LocalizedError {
    let message: String
    var errorDescription: String? { message }
    init(_ message: String) { self.message = message }
}

struct Registration: Decodable {
    struct Installed: Decodable { let client_id: String; let client_secret: String }
    let installed: Installed
}
struct Credential: Codable { let account: String; let token: String }

enum Drive {
    static let manager = FileManager.default
    static let home = manager.homeDirectoryForCurrentUser
    static let support = home.appendingPathComponent("Library/Application Support/MirageSSD")
    static let cache = support.appendingPathComponent("cache")
    static let mount = home.appendingPathComponent("MirageSSD")
    static let label = "org.miragessd.mount"
    static let agent = home.appendingPathComponent("Library/LaunchAgents/\(label).plist")
    static let service = "org.miragessd.drive"

    static func keychain() throws -> Credential? {
        let query: [String: Any] = [kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service, kSecAttrAccount as String: "drive.file",
            kSecReturnData as String: true, kSecMatchLimit as String: kSecMatchLimitOne]
        var result: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        if status == errSecItemNotFound { return nil }
        guard status == errSecSuccess, let data = result as? Data else {
            throw Failure("Unlock your login Keychain and allow MirageSSD access (status \(status)).")
        }
        return try JSONDecoder().decode(Credential.self, from: data)
    }

    static func save(_ value: Credential) throws {
        let query: [String: Any] = [kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service, kSecAttrAccount as String: "drive.file"]
        let data = try JSONEncoder().encode(value)
        let status = SecItemUpdate(query as CFDictionary, [kSecValueData as String: data] as CFDictionary)
        if status == errSecItemNotFound {
            var added = query
            added[kSecValueData as String] = data
            guard SecItemAdd(added as CFDictionary, nil) == errSecSuccess else { throw Failure("Cannot save Google login in Keychain.") }
        } else if status != errSecSuccess { throw Failure("Cannot update Google login in Keychain.") }
    }

    static func registration() throws -> Registration.Installed {
        guard let url = Bundle.main.url(forResource: "oauth-desktop", withExtension: "json") else {
            throw Failure("This build is missing its Desktop OAuth application registration.")
        }
        let value = try JSONDecoder().decode(Registration.self, from: Data(contentsOf: url)).installed
        guard value.client_id.hasSuffix(".apps.googleusercontent.com"), !value.client_secret.isEmpty else {
            throw Failure("Invalid Desktop OAuth application registration.")
        }
        return value
    }

    static func provider() throws -> URL {
        guard let url = Bundle.main.executableURL?.deletingLastPathComponent().appendingPathComponent("rclone"),
              manager.isExecutableFile(atPath: url.path) else {
            throw Failure("The bundled provider is missing. Reinstall the application.")
        }
        return url
    }

    static func environment(_ token: String? = nil) throws -> [String: String] {
        var env = ProcessInfo.processInfo.environment.filter { !$0.key.uppercased().hasPrefix("RCLONE_") }
        let app = try registration()
        env["RCLONE_CONFIG_MIRAGESSD_TYPE"] = "drive"
        env["RCLONE_CONFIG_MIRAGESSD_CLIENT_ID"] = app.client_id
        env["RCLONE_CONFIG_MIRAGESSD_CLIENT_SECRET"] = app.client_secret
        env["RCLONE_CONFIG_MIRAGESSD_SCOPE"] = "drive.file"
        env["RCLONE_CONFIG_MIRAGESSD_TOKEN"] = token
        return env
    }

    static func command(_ executable: URL, _ arguments: [String], env: [String: String]? = nil,
                        timeout: Double = 60) throws -> Data {
        let process = Process()
        process.executableURL = executable
        process.arguments = arguments
        process.environment = env
        let pipe = Pipe()
        process.standardOutput = pipe
        // Authorization output contains tokens: never forward stdout or stderr to logs.
        process.standardError = FileHandle.nullDevice
        process.standardInput = FileHandle.nullDevice
        try process.run()
        let stop = DispatchWorkItem { if process.isRunning { process.terminate() } }
        DispatchQueue.global().asyncAfter(deadline: .now() + timeout, execute: stop)
        defer { stop.cancel(); if process.isRunning { process.terminate() } }
        var output = Data()
        while let part = try pipe.fileHandleForReading.read(upToCount: 4096), !part.isEmpty {
            guard output.count + part.count <= 65536 else { throw Failure("Provider response exceeded its limit.") }
            output.append(part)
        }
        process.waitUntilExit()
        guard process.terminationStatus == 0 else { throw Failure("Operation failed or timed out (exit \(process.terminationStatus)). Check connectivity, Google access, and macFUSE installation.") }
        return output
    }

    static func connect() throws {
        let old = try keychain()
        let staged = try manager.fileExists(atPath: cache.path) ? manager.contentsOfDirectory(atPath: cache.path) : []
        if old == nil, !staged.isEmpty {
            throw Failure("An existing cache has no Keychain account binding. Preserve it and recover the original login before reconnecting.")
        }
        let app = try registration()
        let parameters = ["client_id": app.client_id, "client_secret": app.client_secret, "scope": "drive.file"]
        let encoded = try JSONSerialization.data(withJSONObject: parameters).base64EncodedString().replacingOccurrences(of: "=", with: "")
        let output = try command(provider(), ["authorize", "drive", encoded, "--config", "/dev/null"], env: environment(), timeout: 600)
        guard let text = String(data: output, encoding: .utf8),
              let start = text.range(of: "--->\n"), let end = text.range(of: "\n<---End paste", range: start.upperBound..<text.endIndex) else {
            throw Failure("The provider did not return an authorization response.")
        }
        var blob = String(text[start.upperBound..<end.lowerBound]).trimmingCharacters(in: .whitespacesAndNewlines)
        blob += String(repeating: "=", count: (4 - blob.count % 4) % 4)
        guard let data = Data(base64Encoded: blob),
              let config = try JSONSerialization.jsonObject(with: data) as? [String: String],
              let token = config["token"], let tokenData = token.data(using: .utf8),
              let fields = try JSONSerialization.jsonObject(with: tokenData) as? [String: Any],
              let access = fields["access_token"] as? String,
              let refresh = fields["refresh_token"] as? String, !refresh.isEmpty else {
            throw Failure("Google did not return an offline login. Reconnect and approve access.")
        }
        // Verify account identity before replacing a cache's existing binding.
        var request = URLRequest(url: URL(string: "https://www.googleapis.com/drive/v3/about?fields=user(permissionId)")!)
        request.setValue("Bearer \(access)", forHTTPHeaderField: "Authorization")
        request.timeoutInterval = 30
        let semaphore = DispatchSemaphore(value: 0)
        var account: String?
        let session = URLSession(configuration: .ephemeral)
        let task = session.dataTask(with: request) { data, response, _ in
            defer { semaphore.signal() }
            if (response as? HTTPURLResponse)?.statusCode == 200, let data = data, data.count <= 65536,
               let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
               let user = json["user"] as? [String: Any] { account = user["permissionId"] as? String }
        }
        task.resume()
        guard semaphore.wait(timeout: .now() + 35) == .success, let identity = account, !identity.isEmpty else {
            task.cancel(); session.invalidateAndCancel(); throw Failure("Could not verify the Google account. Existing credentials are unchanged.")
        }
        session.finishTasksAndInvalidate()
        if let old = old, old.account != identity { throw Failure("That is a different Google account. The original cache and login were preserved.") }
        try save(Credential(account: identity, token: token))
    }

    static func directory(_ path: URL) throws {
        if manager.fileExists(atPath: path.path) {
            let values = try path.resourceValues(forKeys: [.isSymbolicLinkKey, .isDirectoryKey])
            guard values.isDirectory == true, values.isSymbolicLink != true else { throw Failure("Unsafe application directory: \(path.lastPathComponent)") }
        } else { try manager.createDirectory(at: path, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700]) }
        try manager.setAttributes([.posixPermissions: 0o700], ofItemAtPath: path.path)
    }

    static func mountDrive() throws {
        guard manager.fileExists(atPath: "/Library/Filesystems/macfuse.fs") else { throw Failure("Install and approve macFUSE, then restart if requested.") }
        guard let credential = try keychain() else { throw Failure("Open MirageSSD and connect Google Drive first.") }
        try directory(support); try directory(cache)
        // Never mount over existing user files or a symlink.
        try directory(mount)
        guard (try manager.contentsOfDirectory(atPath: mount.path)).isEmpty else { throw Failure("Mount folder is not empty or is already mounted.") }
        let process = Process()
        process.executableURL = try provider()
        process.environment = try environment(credential.token)
        process.arguments = ["mount", "miragessd:MirageSSD Storage", mount.path, "--config", "/dev/null",
            "--volname", "MirageSSD", "--vfs-cache-mode", "full", "--cache-dir", cache.path,
            "--vfs-cache-max-size", "20Gi", "--vfs-cache-min-free-space", "5Gi",
            "--vfs-write-back", "5s", "--vfs-read-ahead", "16Mi", "--vfs-read-chunk-size", "8Mi",
            "--vfs-read-chunk-streams", "4", "--transfers", "4", "--buffer-size", "8Mi"]
        process.standardInput = FileHandle.nullDevice
        process.standardOutput = FileHandle.nullDevice
        process.standardError = FileHandle.nullDevice
        try process.run()
        process.waitUntilExit()
        guard process.terminationStatus == 0 else { throw Failure("Mount failed (exit \(process.terminationStatus)); reconnect Google or check macFUSE.") }
    }

    static func installStartup() throws {
        let app = Bundle.main.bundleURL.standardizedFileURL.path
        let userApps = home.appendingPathComponent("Applications").path + "/"
        guard app.hasPrefix("/Applications/") || app.hasPrefix(userApps) else {
            throw Failure("Move MirageSSD.app to Applications before enabling login startup.")
        }
        guard let executable = Bundle.main.executableURL else { throw Failure("Missing app executable.") }
        try directory(support)
        try manager.createDirectory(at: agent.deletingLastPathComponent(), withIntermediateDirectories: true)
        let plist: [String: Any] = ["Label": label, "ProgramArguments": [executable.path, "--mount"],
            "RunAtLoad": true, "KeepAlive": ["SuccessfulExit": false], "ThrottleInterval": 60,
            "ProcessType": "Background", "LimitLoadToSessionType": "Aqua"]
        let bytes = try PropertyListSerialization.data(fromPropertyList: plist, format: .xml, options: 0)
        try bytes.write(to: agent, options: .atomic)
        try manager.setAttributes([.posixPermissions: 0o600], ofItemAtPath: agent.path)
        // Do not bootout/kickstart an existing mount: pending writes may exist.
        let domain = "gui/\(getuid())"
        if (try? command(URL(fileURLWithPath: "/bin/launchctl"), ["print", "\(domain)/\(label)"])) == nil {
            _ = try command(URL(fileURLWithPath: "/bin/launchctl"), ["bootstrap", domain, agent.path])
        }
    }
}

final class AppDelegate: NSObject, NSApplicationDelegate {
    var window: NSWindow!
    let status = NSTextField(wrappingLabelWithString: "Connect your Google Drive, then enable the Finder mount. macFUSE must be installed first.")
    var buttons = [NSButton]()
    func applicationDidFinishLaunching(_ notification: Notification) {
        window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 510, height: 310), styleMask: [.titled, .closable, .miniaturizable], backing: .buffered, defer: false)
        window.title = "MirageSSD — macOS Preview"
        let stack = NSStackView()
        stack.orientation = .vertical; stack.alignment = .leading; stack.spacing = 16
        stack.translatesAutoresizingMaskIntoConstraints = false
        let title = NSTextField(labelWithString: "Your Drive, in Finder")
        title.font = .boldSystemFont(ofSize: 23)
        stack.addArrangedSubview(title); stack.addArrangedSubview(status)
        for (title, action) in [("Connect / reconnect Google", #selector(connect)), ("Enable drive at login", #selector(enable)), ("Open mounted folder", #selector(openFolder)), ("Install macFUSE…", #selector(runtime))] {
            let button = NSButton(title: title, target: self, action: action)
            button.bezelStyle = .rounded; buttons.append(button); stack.addArrangedSubview(button)
        }
        window.contentView!.addSubview(stack)
        NSLayoutConstraint.activate([stack.leadingAnchor.constraint(equalTo: window.contentView!.leadingAnchor, constant: 24), stack.trailingAnchor.constraint(equalTo: window.contentView!.trailingAnchor, constant: -24), stack.topAnchor.constraint(equalTo: window.contentView!.topAnchor, constant: 24)])
        window.center(); window.makeKeyAndOrderFront(nil); NSApp.activate(ignoringOtherApps: true)
    }
    func work(_ operation: @escaping () throws -> Void, success: String) {
        buttons.forEach { $0.isEnabled = false }; status.stringValue = "Working… Google sign-in may open in your browser."
        DispatchQueue.global().async {
            let message: String
            do { try operation(); message = success } catch { message = error.localizedDescription }
            DispatchQueue.main.async { self.status.stringValue = message; self.buttons.forEach { $0.isEnabled = true } }
        }
    }
    @objc func connect() { work({ try Drive.connect() }, success: "Google login saved in Keychain. Enable the drive at login next.") }
    @objc func enable() { work({ try Drive.installStartup() }, success: "Login startup enabled. The background mount retries failures; this is not proof uploads are complete.") }
    @objc func openFolder() { NSWorkspace.shared.open(Drive.mount) }
    @objc func runtime() { NSWorkspace.shared.open(URL(string: "https://macfuse.github.io/")!) }
    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { true }
}

if CommandLine.arguments.contains("--mount") {
    do { try Drive.mountDrive() } catch { fputs("MirageSSD: \(error.localizedDescription)\n", stderr); exit(1) }
} else if CommandLine.arguments.contains("--check-package") {
    do { _ = try Drive.registration(); _ = try Drive.provider(); print("Package resources present; no mount or authentication performed.") } catch { exit(1) }
} else {
    let app = NSApplication.shared
    let delegate = AppDelegate()
    app.setActivationPolicy(.regular); app.delegate = delegate; app.run()
}
