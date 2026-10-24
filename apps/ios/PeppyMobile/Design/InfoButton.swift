import SwiftUI

/// A neutral, contextual control that presents localized explanatory copy.
struct InfoButton: View {
    let topic: String
    let title: String
    let helpBody: String
    let id: String

    @State private var isPresented = false

    init(topic: String, title: String, body: String, id: String) {
        self.topic = topic
        self.title = title
        helpBody = body
        self.id = id
    }

    private var moreAboutLabel: String {
        let format = String(localized: "peppy.help_ios_more_about", table: "Peppy")
        return String(format: format, locale: .current, topic)
    }

    var body: some View {
        Button { isPresented = true } label: {
            Image(systemName: "info.circle")
                .font(.body)
                .frame(minWidth: 44, minHeight: 44)
        }
        .foregroundStyle(.secondary)
        .accessibilityLabel(Text(verbatim: moreAboutLabel))
        .accessibilityIdentifier(id)
        .popover(isPresented: $isPresented) {
            NavigationStack {
                ScrollView {
                    VStack(alignment: .leading, spacing: 16) {
                        Text(title)
                            .font(.headline)
                        Text(helpBody)
                            .font(.body)
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(24)
                }
                .navigationTitle(title)
                .navigationBarTitleDisplayMode(.inline)
                .toolbar {
                    ToolbarItem(placement: .topBarTrailing) {
                        Button(String(localized: "peppy.help_dismiss", table: "Peppy")) {
                            isPresented = false
                        }
                        .accessibilityIdentifier("\(id)-dismiss")
                    }
                }
            }
            .accessibilityIdentifier("\(id)-popover")
            .presentationCompactAdaptation(.popover)
            .frame(idealWidth: 400, maxWidth: 500, idealHeight: 280, maxHeight: 500)
        }
    }
}
