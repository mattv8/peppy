import Foundation
import SwiftUI
#if canImport(Contacts)
import Contacts
#endif
#if canImport(UIKit)
import UIKit
#endif
#if canImport(PeppyNative)
import PeppyNative
#endif

/// UI state for contact sync. Domain state comes from the core via `ContactsOverview`.
struct ContactsState {
    var enabled = false
    var intent: ContactsSyncIntent = .unset
    var access: ContactsAccess = .notDetermined
    var overview = ContactsOverview()
    var lastReport: ContactsSyncReport?
    var lastPassDate: Date?
    var didReconcileAccess = false
    var working = false
    var error: String?
}

private struct ContactsConsentSheet: View {
    let onDismiss: () -> Void
    let onAgree: () -> Void
    @Environment(\.colorScheme) private var colorScheme

    var body: some View {
        VStack(spacing: 0) {
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    Text("peppy.toggle_ios_contacts_consent_title", tableName: "Peppy").font(.title2.bold())
                        .accessibilityAddTraits(.isHeader).accessibilityIdentifier("contacts-consent-sheet-title")
                    Text("peppy.contacts_description", tableName: "Peppy").accessibilityIdentifier("contacts-consent-sheet-body")
                    Text("peppy.toggle_ios_contacts_data_flow", tableName: "Peppy")
                    Text("peppy.toggle_ios_contacts_scope", tableName: "Peppy").accessibilityIdentifier("contacts-consent-sheet-next-step")
                }.padding(24)
            }
            Divider()
            VStack(spacing: 8) {
                Button { onAgree() } label: { Text("peppy.toggle_ios_agree", tableName: "Peppy").frame(maxWidth: .infinity, minHeight: 44) }
                    .buttonStyle(.borderedProminent).tint(PeppyTokens.colors(for: colorScheme).Accent).foregroundStyle(PeppyTokens.colors(for: colorScheme).AccentText)
                    .accessibilityIdentifier("contacts-consent-sheet-confirm")
                Button { onDismiss() } label: { Text("peppy.toggle_ios_not_now", tableName: "Peppy").frame(maxWidth: .infinity, minHeight: 44) }
                    .buttonStyle(.bordered).accessibilityIdentifier("contacts-consent-sheet-dismiss")
            }.padding(16)
        }.accessibilityIdentifier("contacts-consent-sheet").presentationDetents([.medium, .large]).presentationDragIndicator(.visible)
    }
}

extension AppModel {
    /// Contact opt-in belongs to the enrolled identity. Teardown stops local work and records an
    /// explicit stopped state so a different enrollment must opt in again.
    func stopContactsForIdentityTeardown() async {
        await contactPasses.cancel()
        contactsPreferences.save(ContactsPreferences(intent: .stopped))
        contacts.enabled = false
        contacts.intent = .stopped
        contacts.lastReport = nil
    }

    /// Reconciles local contact access and runs one bounded foreground pass when access starts.
    func refreshContacts() async {
        let previousAccess = contacts.access
        contacts.access = (try? await contactsProvider.access()) ?? .notDetermined
        let preferences = contactsPreferences.load()
        contacts.enabled = preferences.enabled
        contacts.intent = preferences.intent
        if status.databaseOpen { contacts.overview = (try? await session.contactsOverview()) ?? contacts.overview }
        let regainedAccess = !previousAccess.mayRead && contacts.access.mayRead && preferences.intent == .on && contacts.lastReport?.outcome == .noAccess
        let initialForegroundPass = !contacts.didReconcileAccess && contacts.access.mayRead && preferences.intent == .on
        contacts.didReconcileAccess = true
        if status.databaseOpen, (initialForegroundPass || regainedAccess), contacts.enabled {
            await runContactsPass(.foreground)
        }
    }

    /// Explicit user action only: asks for Contacts access in the foreground, then opts in.
    func enableContacts(afterAccess: @MainActor (Bool) -> Bool = { $0 }) async {
        contacts.working = true
        defer { contacts.working = false }
        #if canImport(Contacts)
        if (try? await contactsProvider.access()) == .notDetermined {
            _ = try? await CNContactStore().requestAccess(for: .contacts)
        }
        #endif
        contacts.access = (try? await contactsProvider.access()) ?? .notDetermined
        guard contacts.access.mayRead, afterAccess(contacts.access.mayRead) else { return }
        var preferences = contactsPreferences.load()
        preferences.intent = .on
        contactsPreferences.save(preferences)
        let readback = contactsPreferences.load()
        contacts.enabled = readback.enabled
        contacts.intent = readback.intent
        guard readback.enabled else { return }
        scheduleContactsBackgroundWork()
        await runContactsPass(.foreground)
    }

    /// Stops syncing without publishing anything; the book stays as last published.
    func pauseContacts() {
        var preferences = contactsPreferences.load()
        preferences.enabled = false
        contactsPreferences.save(preferences)
        contacts.enabled = false
        contacts.intent = .stopped
    }

    /// The new-contact account is an owner setting in the core, not a phone preference.
    func setDefaultAccount(_ id: String) async {
        await contactsAction { try await $0.setContactsDefaultAccount(id) }
    }

    func setRemoteEdits(_ mode: String) async {
        await contactsAction { try await $0.setContactsRemoteEdits(mode) }
    }

    /// Approval is recorded locally; the next pass takes the one-use permit and writes.
    func decideContactRequest(_ request: ContactsPendingRequest, approve: Bool) async {
        await contactsAction { try await $0.decideContactRequest(request.id, approve: approve, scanHold: request.isScanHold) }
        if approve { await runContactsPass(.foreground) }
    }

    func retireContacts() async {
        let preferences = contactsPreferences
        await contactsAction { try await $0.retireContacts(preferences: preferences) }
        contacts.enabled = contactsPreferences.load().enabled
        contacts.intent = contactsPreferences.load().intent
        contacts.lastReport = nil
    }

    func runContactsPass(_ reason: ContactsPassReason) async {
        guard status.databaseOpen else { return }
        do {
            try await contactPasses.run(reason)
        } catch ClientError.canceled {
        } catch is CancellationError {
        } catch {
            contacts.error = (error as? ClientError)?.userMessage ?? "Contact sync failed."
        }
    }

    func contactsPassFinished(_ report: ContactsSyncReport) async {
        await publishServerReachability()
        contacts.lastReport = report
        if report.outcome != .busy && report.outcome != .disabled { contacts.lastPassDate = Date() }
        contacts.error = report.syncError
        if report.remoteScanPending, contacts.enabled, !remoteScanFollowUpQueued {
            remoteScanFollowUpQueued = true
            scheduleContactsBackgroundWork()
            await runContactsPass(.changeNotification)
        } else if !report.remoteScanPending {
            remoteScanFollowUpQueued = false
        }
        await refreshContacts()
    }

    /// Foreground-only change observation; background freshness comes from BackgroundTasks.
    func observeContactChanges(_ active: Bool) {
        #if canImport(Contacts)
        if active, contactsObserver == nil {
            contactsObserver = NotificationCenter.default.addObserver(
                forName: .CNContactStoreDidChange, object: nil, queue: .main
            ) { [weak self] _ in
                Task { @MainActor in await self?.runContactsPass(.changeNotification) }
            }
        } else if !active, let observer = contactsObserver {
            NotificationCenter.default.removeObserver(observer)
            contactsObserver = nil
        }
        #endif
    }

    private func contactsAction(_ body: (NativeSession) async throws -> ContactsOverview) async {
        contacts.working = true
        defer { contacts.working = false }
        do {
            contacts.overview = try await body(session)
            contacts.error = nil
        } catch {
            contacts.error = (error as? ClientError)?.userMessage ?? "The contact setting could not be saved."
        }
    }
}

/// Contacts settings: opt-in, access, new-contact container, remote edit policy, last pass,
/// pending approvals and retire. Never promises a sync latency.
struct ContactsSection: View {
    let model: AppModel
    @State private var confirmingRetire = false
    @State private var showingStopOptions = false
    @State private var consent = ContactsConsentDecision()
    @State private var showingConsent = false
    @State private var awaitingSettingsReturn = false
    @Environment(\.scenePhase) private var scenePhase

    var body: some View {
        Section {
            Toggle(isOn: Binding(get: { model.contacts.enabled }, set: { setEnabled($0) })) {
                VStack(alignment: .leading, spacing: 2) {
                    Text("peppy.toggle_ios_contacts_title", tableName: "Peppy")
                    Text(model.contacts.enabled
                         ? String(localized: "peppy.toggle_ios_contacts_on", table: "Peppy")
                         : String(localized: "peppy.toggle_ios_contacts_off", table: "Peppy"))
                        .font(.footnote).foregroundStyle(.secondary)
                }
            }
            .disabled(model.contacts.working || !model.status.keysUnlocked || (model.contacts.access == .restricted && !model.contacts.enabled))
            .accessibilityIdentifier("contacts-sync-row")
            if model.contacts.access == .restricted {
                Text("peppy.toggle_ios_contacts_restricted", tableName: "Peppy")
                    .font(.footnote).foregroundStyle(.secondary)
                    .accessibilityIdentifier("contacts-restricted-reason")
            }
            if let attention = attentionSummary {
                Text(attention).foregroundStyle(PeppyTokens.colors(for: colorScheme).Error)
                    .accessibilityIdentifier("contacts-attention-summary")
            }
            NavigationLink { contactsDetails } label: {
                Text("peppy.toggle_ios_contacts_details", tableName: "Peppy")
            }
            .accessibilityIdentifier("contacts-details-link")
        } header: {
            HStack {
                Text("Contacts")
                InfoButton(topic: String(localized: "peppy.help_ios_contacts_title", table: "Peppy"), title: String(localized: "peppy.help_ios_contacts_title", table: "Peppy"), body: String(localized: "peppy.help_ios_contacts", table: "Peppy"), id: "info-contacts")
                Spacer()
            }
        }
        .accessibilityIdentifier("contacts-section")
        .task(id: model.status.databaseOpen) { await model.refreshContacts() }
        .onChange(of: scenePhase) { _, phase in
            guard phase == .active, awaitingSettingsReturn else { return }
            awaitingSettingsReturn = false
            Task { await completeSettingsReturn() }
        }
        .onDisappear { awaitingSettingsReturn = false; consent.cancel() }
        .sheet(isPresented: $showingConsent, onDismiss: { if consent.phase == .disclosure { consent.cancel() } }) {
            ContactsConsentSheet(onDismiss: { showingConsent = false }, onAgree: agree)
        }
        .confirmationDialog("Retire this contact book?", isPresented: $confirmingRetire, titleVisibility: .visible) {
            Button("Retire", role: .destructive) { Task { await model.retireContacts() } }
                .accessibilityIdentifier("contacts-confirm-retire-button")
        } message: {
            Text("Your other devices will show this book as retired. Contacts on this phone are not changed or deleted. Turning sync on again starts a new book.")
        }
        .confirmationDialog("Turn off contact sync?", isPresented: $showingStopOptions, titleVisibility: .visible) {
            Button { model.pauseContacts() } label: { Text("peppy.toggle_ios_contacts_pause", tableName: "Peppy") }
                .accessibilityIdentifier("contacts-pause-button")
            Button(role: .destructive) { confirmingRetire = true } label: { Text("peppy.toggle_ios_contacts_retire", tableName: "Peppy") }
                .disabled(model.contacts.overview.bookId == nil)
                .accessibilityIdentifier("contacts-retire-button")
            Button(role: .cancel) {} label: { Text("peppy.cancel", tableName: "Peppy") }
        } message: {
            Text("peppy.toggle_ios_contacts_stop_consequences", tableName: "Peppy")
        }
    }

    @Environment(\.colorScheme) private var colorScheme

    private var attentionSummary: String? {
        if let error = model.contacts.error { return error }
        if let report = model.contacts.lastReport, let outcome = Self.outcomeText(report) { return outcome }
        if !model.contacts.overview.pending.isEmpty { return String(localized: "peppy.toggle_ios_contacts_status", table: "Peppy") }
        return nil
    }

    private func setEnabled(_ enabled: Bool) {
        if enabled { beginConsent() } else if model.contacts.enabled { showingStopOptions = true }
    }

    private var contactsDetails: some View {
        Form {
            Section {
                accessRows
                containerPicker
                policyPicker
                statusRows
                approvals
            } header: {
                Text("peppy.toggle_ios_contacts_section", tableName: "Peppy")
            }
        }
        .navigationTitle(Text("peppy.toggle_ios_contacts_title", tableName: "Peppy"))
        .accessibilityIdentifier("contacts-details-screen")
    }

    private func beginConsent() {
        guard !model.contacts.working, model.status.keysUnlocked else { return }
        consent.requestEnable()
        showingConsent = consent.phase == .disclosure
    }

    private func agree() {
        showingConsent = false
        switch consent.agree(access: model.contacts.access) {
        case .apply:
            Task { await applyIfCurrent() }
        case .requestAccess:
            Task {
                await model.enableContacts(afterAccess: { granted in
                    consent.accessResult(granted: granted) == .apply && model.status.keysUnlocked
                })
                consent.finish()
            }
        case .openSettings:
            awaitingSettingsReturn = true
            openAppSettings()
        case .none:
            break
        }
    }

    private func applyIfCurrent() async {
        guard consent.phase == .applying, model.status.keysUnlocked, !model.contacts.working else { return }
        await model.enableContacts(afterAccess: { _ in consent.phase == .applying && model.status.keysUnlocked })
        consent.finish()
    }

    private func completeSettingsReturn() async {
        await model.refreshContacts()
        guard model.status.keysUnlocked,
              consent.settingsAccessResult(granted: model.contacts.access.mayRead) == .apply
        else { return }
        await applyIfCurrent()
    }

    private func openAppSettings() {
        #if canImport(UIKit)
        if let url = URL(string: UIApplication.openSettingsURLString) { UIApplication.shared.open(url) }
        #endif
    }

    @ViewBuilder private var accessRows: some View {
        LabeledContent("Access", value: Self.accessText(model.contacts.access))
            .accessibilityIdentifier("contacts-access-row")
        switch model.contacts.access {
        case .limited, .denied, .restricted:
            Text(model.contacts.access == .limited
                 ? "Only the contacts you selected are synced. Contacts outside the selection are never treated as deleted."
                 : "Contacts are unavailable. Your other devices keep the last synced book and see it as unavailable.")
            #if canImport(UIKit)
            Button("Manage access in Settings") {
                if let url = URL(string: UIApplication.openSettingsURLString) { UIApplication.shared.open(url) }
            }
            .accessibilityIdentifier("contacts-manage-access-button")
            #endif
        case .notDetermined:
            Button(String(localized: "peppy.contacts_allow_access_ios", table: "Peppy")) { beginConsent() }
                .accessibilityIdentifier("contacts-request-access-button")
        case .full:
            EmptyView()
        }
    }

    @ViewBuilder private var containerPicker: some View {
        let accounts = model.contacts.overview.accounts.filter(\.writable)
        if !accounts.isEmpty {
            Picker("Save new contacts to", selection: Binding(
                get: { model.contacts.overview.defaultAccountId ?? accounts.first?.id ?? "" },
                set: { id in if !id.isEmpty { Task { await model.setDefaultAccount(id) } } }
            )) {
                ForEach(accounts) { account in
                    Text(account.name).tag(account.id)
                }
            }
            .accessibilityIdentifier("contacts-container-picker")
        }
    }

    @ViewBuilder private var policyPicker: some View {
        if model.contacts.overview.bookId != nil {
            Picker("Edits from other devices", selection: Binding(
                get: { model.contacts.overview.remoteEdits ?? "confirm" },
                set: { mode in Task { await model.setRemoteEdits(mode) } }
            )) {
                Text("Apply automatically").tag("auto")
                Text("Ask me first").tag("confirm")
                Text("Off").tag("off")
            }
            .accessibilityIdentifier("contacts-remote-edits-picker")
        }
    }

    @ViewBuilder private var statusRows: some View {
        if let count = model.contacts.overview.contactCount {
            LabeledContent("Contacts in book", value: "\(count)")
        }
        if let date = model.contacts.lastPassDate {
            LabeledContent("Last checked", value: date.formatted(.relative(presentation: .named)))
                .accessibilityIdentifier("contacts-last-pass-row")
        } else {
            Text("Not checked yet in this session.")
        }
        if let report = model.contacts.lastReport {
            if let text = Self.outcomeText(report) { Text(text).accessibilityIdentifier("contacts-outcome-text") }
            if report.held > 0 {
                Text("\(report.held) contacts disappeared at once; removing them from your other devices is on hold.")
            }
            if report.limitExceeded { Text("peppy.contacts_book_limit", tableName: "Peppy") }
            if report.photoFailures > 0 {
                Text("\(report.photoFailures) contact photo(s) could not be prepared.")
                    .accessibilityIdentifier("contacts-photo-failures")
            }
            if report.media.unavailable > 0 {
                Text("\(report.media.unavailable) contact photo(s) are unavailable on the server and will not be retried immediately.")
                    .accessibilityIdentifier("contacts-photo-unavailable")
            }
            if report.media.failed > 0 {
                Text("\(report.media.failed) contact photo transfer(s) failed; they will be retried later.")
                    .accessibilityIdentifier("contacts-photo-transfer-failures")
            }
            if report.retentionPaused {
                Text("Contact photo retention is paused until all vault devices support active compaction.")
                    .accessibilityIdentifier("contacts-retention-paused")
            }
        }
        if model.contacts.working { ProgressView() }
    }

    @ViewBuilder private var approvals: some View {
        let pending = model.contacts.overview.pending
        if !pending.isEmpty {
            ForEach(pending) { request in
                VStack(alignment: .leading) {
                    Text(Self.requestText(request))
                    if let detail = Self.requestDetail(request) { Text(detail).font(.footnote).foregroundStyle(.secondary) }
                    if request.state == "requested" || request.state == "awaiting_approval" {
                        HStack {
                            Button("Approve") { Task { await model.decideContactRequest(request, approve: true) } }
                                .accessibilityIdentifier("contacts-approve-\(request.id)")
                            Button("Reject", role: .destructive) { Task { await model.decideContactRequest(request, approve: false) } }
                                .accessibilityIdentifier("contacts-reject-\(request.id)")
                        }
                        .buttonStyle(.borderless)
                    }
                }
                .accessibilityElement(children: .contain)
                .accessibilityIdentifier("contacts-request-\(request.id)")
            }
        }
    }

    static func accessText(_ access: ContactsAccess) -> String {
        switch access {
        case .full: "All contacts"
        case .limited: "Selected contacts"
        case .denied: "Denied"
        case .restricted: "Restricted"
        case .notDetermined: "Not requested"
        }
    }

    static func outcomeText(_ report: ContactsSyncReport) -> String? {
        switch report.outcome {
        case .needsUnlock: "Waiting for the vault to be unlocked on this phone."
        case .busy: "Another sync was running; contacts are checked next."
        case .serverIncompatible: "This server cannot safely compact contact history; contacts are paused."
        case .backfillPending: "Preparing existing contact history for safe sync; contacts resume on a later check."
        case .noAccess: "Contacts access is off; nothing was removed from your other devices."
        case .partial: "The last check was interrupted or partial; nothing was removed. It continues next time."
        case .repairPending: report.projectionFailure.map { "Rebuilding contacts from the server failed (\($0)); it is retried on the next check." }
            ?? "Contacts are being rebuilt from the server before this phone publishes changes."
        case .unchanged, .incremental, .completed, .disabled:
            report.photoWorkRemaining ? "Contact photos are still being checked; this continues on later checks." : nil
        }
    }

    static func requestText(_ request: ContactsPendingRequest) -> String {
        if request.isScanHold {
            return "\(request.count ?? 0) contacts disappeared at once. Remove them from your other devices?"
        }
        let name = request.displayName ?? "a contact"
        switch request.state {
        case "requested", "awaiting_approval":
            return switch request.kind {
            case "create": "Another device wants to add \(name)"
            case "delete": "Another device wants to delete \(name)"
            default: "Another device wants to edit \(name)"
            }
        case "approved": return "Approved; applied on the next contact check"
        case "applying", "outcome_unknown": return "An edit to \(name) may or may not have been saved; check this contact. It will not be retried."
        default: return request.state
        }
    }

    static func requestDetail(_ request: ContactsPendingRequest) -> String? {
        if request.isScanHold { return request.sampleNames.isEmpty ? nil : request.sampleNames.joined(separator: ", ") }
        return request.fieldPaths.isEmpty ? nil : "Fields: " + request.fieldPaths.joined(separator: ", ")
    }
}
