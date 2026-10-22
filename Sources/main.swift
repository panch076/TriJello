import AppKit

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    private var manager: WindowManager!
    private var statusItem: NSStatusItem!

    func applicationDidFinishLaunching(_ notification: Notification) {
        if !CGPreflightScreenCaptureAccess() {
            CGRequestScreenCaptureAccess()
            let alert = NSAlert()
            alert.messageText = "TriJello needs Screen Recording permission"
            alert.informativeText = "Enable TriJello in System Settings → Privacy & Security → Screen & System Audio Recording, then relaunch it."
            alert.runModal()
            NSApp.terminate(nil)
            return
        }

        do {
            manager = WindowManager(renderer: try Renderer())
        } catch {
            fatalError("Metal setup failed: \(error)")
        }
        manager.start()
        buildStatusItem()
    }

    private func buildStatusItem() {
        statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        statusItem.button?.title = "△"
        let menu = NSMenu()
        menu.addItem(toggle("HDR Boost", #selector(toggleHDR), manager.hdrEnabled))
        menu.addItem(toggle("Jello Physics", #selector(toggleJello), manager.jelloEnabled))
        menu.addItem(toggle("Triangles", #selector(toggleTriangles), manager.trianglesEnabled))
        menu.addItem(.separator())
        menu.addItem(NSMenuItem(title: "Quit TriJello", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "q"))
        statusItem.menu = menu
    }

    private func toggle(_ title: String, _ action: Selector, _ on: Bool) -> NSMenuItem {
        let item = NSMenuItem(title: title, action: action, keyEquivalent: "")
        item.target = self
        item.state = on ? .on : .off
        return item
    }

    @objc private func toggleHDR(_ sender: NSMenuItem) {
        manager.hdrEnabled.toggle()
        sender.state = manager.hdrEnabled ? .on : .off
    }

    @objc private func toggleJello(_ sender: NSMenuItem) {
        manager.jelloEnabled.toggle()
        sender.state = manager.jelloEnabled ? .on : .off
    }

    @objc private func toggleTriangles(_ sender: NSMenuItem) {
        manager.trianglesEnabled.toggle()
        sender.state = manager.trianglesEnabled ? .on : .off
    }
}

MainActor.assumeIsolated {
    let app = NSApplication.shared
    let delegate = AppDelegate()
    app.delegate = delegate
    app.setActivationPolicy(.accessory)
    app.run()
}
