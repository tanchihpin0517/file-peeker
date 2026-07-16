import SwiftUI

struct ContentView: View {
    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("File Peeker")
                .font(.title)
            Text("The v1 application skeleton is ready.")
                .foregroundStyle(.secondary)
        }
        .padding(24)
        .frame(minWidth: 520, minHeight: 240, alignment: .topLeading)
    }
}
