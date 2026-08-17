// Tether.app — a menu bar front end for the `tether` daemon.
//
// Why this exists: `tether` is a CLI that needs a subcommand. Wrapping it in an
// app bundle without a UI produced something that launched, printed usage to an
// invisible stderr, and quit — an icon that did nothing. This gives the bundle
// an actual job: pick a role, start and stop the daemon, and show whether macOS
// has granted the permission it cannot run without.
//
// It is a menu bar accessory (no Dock icon, no window). The daemon runs as a
// child process, which is also what makes permissions work: macOS attributes a
// TCC grant to the *responsible* process, so approving Tether.app once covers
// the daemon it spawns. Approving a bare CLI binary instead means re-approving
// whatever terminal you launched it from.

import ApplicationServices
import Cocoa

// MARK: - Daemon control

final class Daemon {
    private var process: Process?

    let logURL = FileManager.default
        .homeDirectoryForCurrentUser
        .appendingPathComponent("Library/Logs/Tether.log")

    var isRunning: Bool { process?.isRunning ?? false }

    /// The bundled binary, next to us in Resources.
    static var executableURL: URL? {
        Bundle.main.resourceURL?.appendingPathComponent("tether")
    }

    func start(role: String, pairing: Bool, onExit: @escaping (Int32) -> Void) throws {
        stop()

        guard let exe = Daemon.executableURL,
              FileManager.default.isExecutableFile(atPath: exe.path) else {
            throw NSError(
                domain: "Tether", code: 1,
                userInfo: [NSLocalizedDescriptionKey:
                    "The bundled tether binary is missing from this app."]
            )
        }

        var args = [role]
        if pairing { args.append("--pair") }

        // Truncate per run: the interesting log is always the current session,
        // and an append-forever file in ~/Library/Logs is a slow leak.
        FileManager.default.createFile(atPath: logURL.path, contents: nil)
        let handle = try FileHandle(forWritingTo: logURL)

        let p = Process()
        p.executableURL = exe
        p.arguments = args
        p.standardOutput = handle
        p.standardError = handle
        p.terminationHandler = { proc in
            try? handle.close()
            DispatchQueue.main.async { onExit(proc.terminationStatus) }
        }

        try p.run()
        process = p
    }

    func stop() {
        guard let p = process, p.isRunning else { process = nil; return }
        // SIGTERM, not SIGKILL: the daemon's shutdown path releases held keys
        // on every client and tells them goodbye. Killing it outright can
        // leave a modifier stuck down on another machine.
        p.terminate()
        // Brief grace period, then insist.
        let deadline = Date().addingTimeInterval(3)
        while p.isRunning && Date() < deadline {
            usleep(50_000)
        }
        if p.isRunning { kill(p.processIdentifier, SIGKILL) }
        process = nil
    }
}

// MARK: - App

final class AppDelegate: NSObject, NSApplicationDelegate {
    private var statusItem: NSStatusItem!
    private let daemon = Daemon()
    private var role = UserDefaults.standard.string(forKey: "role") ?? "client"
    private var pairing = false
    private var lastError: String?

    func applicationDidFinishLaunching(_: Notification) {
        statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        statusItem.button?.image = NSImage(
            systemSymbolName: "cursorarrow.click.2", accessibilityDescription: "Tether"
        )
        statusItem.button?.image?.isTemplate = true
        rebuildMenu()

        // Re-check the Accessibility grant periodically: the user will often
        // grant it while this menu is open, and a stale "not granted" line
        // sends them round the loop a second time.
        Timer.scheduledTimer(withTimeInterval: 2, repeats: true) { [weak self] _ in
            self?.rebuildMenu()
        }
    }

    func applicationWillTerminate(_: Notification) {
        daemon.stop()
    }

    // MARK: Menu

    private func rebuildMenu() {
        let menu = NSMenu()
        let trusted = AXIsProcessTrusted()

        let status: String
        if let error = lastError {
            status = "Error: \(error)"
        } else if daemon.isRunning {
            status = role == "host"
                ? "Running as host — sharing this keyboard"
                : "Running as client — waiting for a host"
        } else {
            status = "Stopped"
        }
        let header = NSMenuItem(title: status, action: nil, keyEquivalent: "")
        header.isEnabled = false
        menu.addItem(header)
        menu.addItem(.separator())

        if !trusted {
            let warn = NSMenuItem(
                title: "⚠ Accessibility not granted",
                action: #selector(grantAccessibility), keyEquivalent: ""
            )
            warn.target = self
            menu.addItem(warn)

            // The confusing case: the app appears in System Settings, switched
            // on, and macOS denies anyway. That happens after an update,
            // because an ad-hoc signature makes the permission depend on a
            // hash of the binary and the update changed it. The stale entry
            // has to be cleared before a new one can be granted, and there is
            // no way for a user to guess that from the settings pane.
            let stale = NSMenuItem(
                title: "Already switched on? Reset it and re-ask",
                action: #selector(resetAccessibility), keyEquivalent: ""
            )
            stale.target = self
            menu.addItem(stale)

            let hint = NSMenuItem(
                title: "(needed after an update — the permission is tied to the build)",
                action: nil, keyEquivalent: ""
            )
            hint.isEnabled = false
            menu.addItem(hint)
            menu.addItem(.separator())
        }

        if daemon.isRunning {
            let stop = NSMenuItem(title: "Stop", action: #selector(stopDaemon), keyEquivalent: "")
            stop.target = self
            menu.addItem(stop)
        } else {
            for (title, r) in [("Start as Host", "host"), ("Start as Client", "client")] {
                let item = NSMenuItem(
                    title: title, action: #selector(startDaemon(_:)), keyEquivalent: ""
                )
                item.target = self
                item.representedObject = r
                item.state = (r == role) ? .on : .off
                menu.addItem(item)
            }

            let pair = NSMenuItem(
                title: "Pair with a new machine on next start",
                action: #selector(togglePairing), keyEquivalent: ""
            )
            pair.target = self
            pair.state = pairing ? .on : .off
            menu.addItem(pair)
        }

        menu.addItem(.separator())

        let log = NSMenuItem(title: "Open Log…", action: #selector(openLog), keyEquivalent: "")
        log.target = self
        menu.addItem(log)

        let quit = NSMenuItem(title: "Quit Tether", action: #selector(quit), keyEquivalent: "q")
        quit.target = self
        menu.addItem(quit)

        statusItem.menu = menu
    }

    // MARK: Actions

    @objc private func startDaemon(_ sender: NSMenuItem) {
        role = (sender.representedObject as? String) ?? "client"
        UserDefaults.standard.set(role, forKey: "role")
        lastError = nil

        do {
            try daemon.start(role: role, pairing: pairing) { [weak self] code in
                guard let self else { return }
                // Exit code 0 is a clean stop; anything else is worth surfacing,
                // because the daemon's own error went to a log file the user is
                // not watching.
                if code != 0 && code != 15 {
                    self.lastError = "daemon exited (\(code)) — see the log"
                }
                self.rebuildMenu()
            }
            pairing = false
        } catch {
            lastError = error.localizedDescription
            present(error.localizedDescription)
        }
        rebuildMenu()
    }

    @objc private func stopDaemon() {
        daemon.stop()
        lastError = nil
        rebuildMenu()
    }

    @objc private func togglePairing() {
        pairing.toggle()
        rebuildMenu()
    }

    @objc private func grantAccessibility() {
        // Prompts once, then opens the pane directly — the prompt does not
        // reappear on later calls, so the deep link is the reliable route.
        let options = [kAXTrustedCheckOptionPrompt.takeUnretainedValue() as String: true]
        _ = AXIsProcessTrustedWithOptions(options as CFDictionary)

        if let url = URL(string:
            "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility") {
            NSWorkspace.shared.open(url)
        }
    }

    /// Clear this app's Accessibility entry so a fresh one can be granted.
    ///
    /// After an update the stored grant refers to the previous build's code
    /// hash. macOS keeps showing the row and keeps denying; toggling it off and
    /// on again does not help, because the entry itself is the stale thing.
    /// Removing it is the only route back, and `tccutil` is the supported way.
    @objc private func resetAccessibility() {
        let bundleID = Bundle.main.bundleIdentifier ?? "dev.tether.Tether"

        let reset = Process()
        reset.executableURL = URL(fileURLWithPath: "/usr/bin/tccutil")
        reset.arguments = ["reset", "Accessibility", bundleID]
        do {
            try reset.run()
            reset.waitUntilExit()
        } catch {
            present("Could not reset the permission: \(error.localizedDescription)")
            return
        }

        if reset.terminationStatus != 0 {
            present(
                "Resetting the permission failed. Remove \"Tether\" by hand in "
                    + "System Settings -> Privacy & Security -> Accessibility, "
                    + "then add it again."
            )
            return
        }

        grantAccessibility()
        rebuildMenu()
    }

    @objc private func openLog() {
        if !FileManager.default.fileExists(atPath: daemon.logURL.path) {
            FileManager.default.createFile(atPath: daemon.logURL.path, contents: nil)
        }
        NSWorkspace.shared.open(daemon.logURL)
    }

    @objc private func quit() {
        daemon.stop()
        NSApp.terminate(nil)
    }

    private func present(_ message: String) {
        let alert = NSAlert()
        alert.messageText = "Tether"
        alert.informativeText = message
        alert.alertStyle = .warning
        alert.runModal()
    }
}

let app = NSApplication.shared
let delegate = AppDelegate()
app.delegate = delegate
// .accessory keeps it out of the Dock and the app switcher: this is a menu bar
// utility, and a Dock icon with no window to show is just clutter.
app.setActivationPolicy(.accessory)
app.run()
