import SwiftUI

struct DesktopsView: View {
    @EnvironmentObject private var store: DesktopStore
    @State private var adding = false

    var body: some View {
        List {
            if let error = store.startError {
                Text(error).foregroundStyle(.red)
            }
            if store.desktops.isEmpty {
                ContentUnavailableView(
                    "No computers yet",
                    systemImage: "display",
                    description: Text("Tap + and enter the IP address of a computer running DisplayAnything.")
                )
            }
            ForEach(store.desktops, id: \.fingerprint) { desktop in
                NavigationLink {
                    ViewerView(desktop: desktop)
                } label: {
                    VStack(alignment: .leading, spacing: 2) {
                        Text(desktop.name).font(.headline)
                        Text(desktop.address.map { "\($0):\(desktop.port)" } ?? "No address saved")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                }
            }
            .onDelete { offsets in
                for index in offsets { store.forget(store.desktops[index]) }
            }
        }
        .navigationTitle("Computers")
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Button {
                    adding = true
                } label: {
                    Label("Add computer", systemImage: "plus")
                }
                .disabled(store.client == nil)
            }
        }
        .sheet(isPresented: $adding, onDismiss: store.refresh) {
            NavigationStack {
                AddDesktopView()
            }
        }
    }
}
