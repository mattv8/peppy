import SwiftUI

struct SetupProgressView: View {
    let progress: SetupProgress

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text("peppy.sync_setup_title", tableName: "Peppy").font(.headline)
            Text(verbatim: progress.phase).font(.subheadline).foregroundStyle(.secondary)
            Text("peppy.toggle_ios_sync_setup_body", tableName: "Peppy")
                .font(.subheadline).foregroundStyle(.secondary)
            switch progress {
            case .downloading(let staged, let expected) where expected > 0:
                ProgressView(value: Double(staged), total: Double(expected))
            case .checking, .downloading, .organizing:
                ProgressView()
            }
            if let count = progress.count {
                Text(verbatim: count).font(.footnote).monospacedDigit()
            }
        }
        // One VoiceOver element reads the whole card once, with the count as its value. The children's own
        // identifiers are not reachable separately under `.combine`; the card carries the testable identifier.
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("sync-setup-card")
        .accessibilityValue(progress.count ?? progress.phase)
    }
}

extension SetupProgress {
    /// The localized phase name, from the Peppy catalog.
    var phase: String {
        switch self {
        case .checking: String(localized: "peppy.sync_setup_phase_checking", table: "Peppy")
        case .downloading: String(localized: "peppy.sync_setup_phase_downloading", table: "Peppy")
        case .organizing: String(localized: "peppy.sync_setup_phase_organizing", table: "Peppy")
        }
    }

    /// The localized count line, or nil when there is no number to show.
    var count: String? {
        switch self {
        case .checking: nil
        case .downloading(let staged, let expected):
            String(
                format: String(localized: "peppy.sync_setup_count_downloading", table: "Peppy"),
                locale: .current, staged.formatted(), expected.formatted()
            )
        case .organizing(let remaining):
            remaining.map {
                String(
                    format: String(localized: "peppy.sync_setup_count_organizing", table: "Peppy"),
                    locale: .current, $0.formatted()
                )
            }
        }
    }
}
