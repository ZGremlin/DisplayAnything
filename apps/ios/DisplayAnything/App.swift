import SwiftUI

@main
struct DisplayAnythingApp: App {
    @StateObject private var store = DesktopStore()

    var body: some Scene {
        WindowGroup {
            NavigationStack {
                DesktopsView()
            }
            .environmentObject(store)
        }
    }
}
