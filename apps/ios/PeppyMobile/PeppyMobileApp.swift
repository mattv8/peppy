import SwiftUI
#if canImport(PeppyNative)
import PeppyNative
#endif

@main
struct PeppyMobileApp: App {
    @State private var model: AppModel
    @UIApplicationDelegateAdaptor(PushWakeDelegate.self) private var pushDelegate

    init() {
        let model = AppModel()
        model.registerBackgroundTasks()
        PushWakeDelegate.install(model: model)
        _model = State(initialValue: model)
    }

    var body: some Scene {
        WindowGroup {
            DeviceView(model: model)
                .onOpenURL { url in
                    _ = GoogleHostedSignIn.handle(url)
                }
        }
    }
}

/// The native gateway shell deliberately owns presentation only. Enrollment, encrypted state,
/// contacts and bounded synchronization remain in `NativeSession`/the Rust core.
struct DeviceView: View {
    let model: AppModel
    @Environment(\.scenePhase) private var scenePhase
    @Environment(\.colorScheme) private var colorScheme
    @State private var passphrase = ""
    @State private var confirmingDisconnect = false
    @State private var devicePendingRemoval: DeviceRosterItem?
    @State private var showingSelfHosted = false
    @State private var showingSelfHostedEnrollment = false
    @State private var showingPhoneEnrollment = false
    @State private var showingOwnerPhoneApproval = false
    @State private var ownerPhoneApprovalHost: OwnerPhoneApprovalHost?
    @State private var showingOwnerComputerApproval = false
    @State private var ownerComputerApprovalHost: OwnerComputerApprovalHost?
    @State private var showingHostedEnrollment = false
    @State private var opensPhoneScannerAfterHostedDismissal = false
    @State private var enrollmentLoaded = false
    @State private var hostedAccountClient: HostedAccountClient?
    @State private var selectedTab = "sync"

    var body: some View {
        Group {
            if !enrollmentLoaded {
                ProgressView().accessibilityIdentifier("enrollment-loading")
            } else if model.status.identity == nil {
                if showingSelfHosted {
                    selfHostedSetup
                } else {
                    welcome
                }
            } else if model.status.databaseOpen && !model.status.keysUnlocked {
                lockScreen
            } else {
                enrolledTabs
            }
        }
        .tint(colors.Accent)
        .task { await model.load(); enrollmentLoaded = true }
        .task(id: "\(scenePhase == .active)-\(model.status.identity != nil)-\(model.status.keysUnlocked)") {
            let active = scenePhase == .active
            model.setForegroundActive(active)
            guard active, !Task.isCancelled else { return }
            await model.reconcileWakeRegistration()
            guard !Task.isCancelled else { return }
            await model.syncInForeground()
        }
        .onChange(of: scenePhase, initial: true) { _, phase in
            model.observeContactChanges(phase == .active)
            model.setForegroundActive(phase == .active)
            if phase == .background { model.scheduleContactsBackgroundWork() }
        }
        .fullScreenCover(isPresented: $showingHostedEnrollment, onDismiss: {
            guard opensPhoneScannerAfterHostedDismissal else { return }
            opensPhoneScannerAfterHostedDismissal = false
            showingPhoneEnrollment = true
        }) {
            HostedEnrollmentView(model: model) {
                opensPhoneScannerAfterHostedDismissal = true
            }
        }
        .fullScreenCover(isPresented: $showingSelfHostedEnrollment) {
            SelfHostedEnrollmentView(model: model, isPresented: $showingSelfHostedEnrollment)
        }
        .sheet(isPresented: $showingPhoneEnrollment) {
            PhoneEnrollmentView(model: model, isPresented: $showingPhoneEnrollment)
        }
        .sheet(isPresented: $showingOwnerPhoneApproval) {
            if let ownerPhoneApprovalHost {
                OwnerPhoneApprovalView(
                    host: ownerPhoneApprovalHost,
                    isPresented: $showingOwnerPhoneApproval,
                    revealSecret: { await model.revealOwnerVaultSecret() }
                )
            }
        }
        .sheet(isPresented: $showingOwnerComputerApproval, onDismiss: {
            ownerComputerApprovalHost = nil
        }) {
            if let ownerComputerApprovalHost {
                OwnerComputerApprovalView(host: ownerComputerApprovalHost, isPresented: $showingOwnerComputerApproval)
            }
        }
    }

    private var colors: PeppyColorScheme { PeppyTokens.colors(for: colorScheme) }

    @ViewBuilder private var welcome: some View {
        ScrollView {
            PeppyGlassSurface(colors: colors) {
                VStack(spacing: 16) {
                    Image(decorative: "PeppyLogo").resizable().scaledToFit().frame(width: 72, height: 72)
                        .font(.system(size: 48)).foregroundStyle(colors.Accent)
                    HStack {
                        Text("peppy.onboarding_headline", tableName: "Peppy").font(.title2).bold()
                        InfoButton(topic: String(localized: "peppy.onboarding_headline", table: "Peppy"), title: String(localized: "peppy.onboarding_headline", table: "Peppy"), body: [String(localized: "peppy.onboarding_body", table: "Peppy"), String(localized: "peppy.onboarding_hosted_sub", table: "Peppy")].joined(separator: "\n\n"), id: "info-onboarding")
                    }

                    Picker(selection: Binding(
                        get: { showingSelfHosted },
                        set: { showingSelfHosted = $0 }
                    )) {
                        Text("peppy.onboarding_mode_hosted", tableName: "Peppy").tag(false)
                            .accessibilityIdentifier("welcome-server-mode-hosted")
                        Text("peppy.onboarding_mode_self_hosted", tableName: "Peppy").tag(true)
                            .accessibilityIdentifier("welcome-server-mode-self-hosted")
                    } label: {
                        Text("peppy.onboarding_mode_selector", tableName: "Peppy")
                    }
                    .pickerStyle(.menu)
                    .accessibilityIdentifier("welcome-server-mode-selector")

                    Button { showingHostedEnrollment = true } label: {
                        Text("peppy.onboarding_hosted_cta", tableName: "Peppy")
                            .frame(maxWidth: .infinity, minHeight: 44)
                    }
                    .buttonStyle(.borderedProminent).tint(colors.Accent).foregroundStyle(colors.AccentText).accessibilityIdentifier("welcome-get-started-button")

                    activityAndError
                }
                .padding(24)
            }
            .frame(maxWidth: 460)
            .padding(.vertical, 24)
        }
        .accessibilityIdentifier("welcome-screen")
    }

    @ViewBuilder private var selfHostedSetup: some View {
        ScrollView {
            PeppyGlassSurface(colors: colors) {
                VStack(spacing: 16) {
                    HStack {
                        Text("peppy.self_hosted_headline", tableName: "Peppy").font(.title2).bold()
                        InfoButton(topic: String(localized: "peppy.self_hosted_headline", table: "Peppy"), title: String(localized: "peppy.self_hosted_headline", table: "Peppy"), body: String(localized: "peppy.self_hosted_body", table: "Peppy"), id: "info-self-hosted")
                    }

                    Button("Set up or connect to a server") { showingSelfHostedEnrollment = true }
                        .buttonStyle(.borderedProminent)
                        .tint(colors.Accent)
                        .foregroundStyle(colors.AccentText)
                        .accessibilityIdentifier("self-hosted-enrollment-button")
                    Button("Scan phone enrollment QR") { showingPhoneEnrollment = true }
                        .buttonStyle(.bordered)
                        .accessibilityIdentifier("phone-enrollment-button")

                    Link(destination: URL(string: "https://github.com/mattv8/peppy#readme")!) {
                        Text("peppy.self_hosted_docs_link", tableName: "Peppy")
                            .frame(minHeight: 44)
                    }
                    .accessibilityIdentifier("self-hosted-docs-link")

                    Button { returnToHostedWelcome() } label: {
                        Text("peppy.back", tableName: "Peppy")
                            .frame(minHeight: 44)
                    }
                    .buttonStyle(.bordered)
                    .accessibilityIdentifier("self-hosted-back-button")

                    activityAndError
                }
                .padding(24)
            }
            .frame(maxWidth: 460)
            .padding(.vertical, 24)
        }
        .accessibilityIdentifier("self-hosted-screen")
    }

    private func returnToHostedWelcome() {
        showingSelfHosted = false
    }



    private var lockScreen: some View {
        PeppyGlassSurface(colors: colors) {
            VStack(spacing: 16) {
                Image(systemName: "lock.fill").font(.system(size: 42)).foregroundStyle(colors.Accent)
                Text("peppy.locked", tableName: "Peppy").font(.title2).bold()
                if let identity = model.status.identity {
                    LabeledContent("Server", value: identity.origin)
                    LabeledContent("Device", value: identity.deviceId)
                }
                SecureField(text: $passphrase) { Text("peppy.passphrase_field", tableName: "Peppy") }
                    .textContentType(.password).textFieldStyle(.roundedBorder)
                    .accessibilityIdentifier("vault-passphrase-field")
                Button {
                    let value = passphrase; passphrase = ""
                    Task { await model.unlock(passphrase: value) }
                } label: { Text("peppy.hosted_unlock_cta", tableName: "Peppy") }.disabled(passphrase.isEmpty || model.activity != .idle)
                    .buttonStyle(.borderedProminent).tint(colors.Accent).foregroundStyle(colors.AccentText)
                    .accessibilityIdentifier("unlock-button")
                activityAndError
            }
            .padding(24)
        }
        .frame(maxWidth: 460)
        .accessibilityIdentifier("lock-screen")
    }

    private var enrolledTabs: some View {
        TabView(selection: $selectedTab) {
            syncTab.tabItem { Label { Text("peppy.toggle_ios_sync_tab", tableName: "Peppy") } icon: { Image(systemName: "arrow.triangle.2.circlepath") } }
                .accessibilityIdentifier("tab-sync")
                .tag("sync")
            accountTab.tabItem { Label { Text("peppy.account", tableName: "Peppy") } icon: { Image(systemName: "person.crop.circle") } }
                .accessibilityIdentifier("tab-account")
                .tag("account")
        }
        .onChange(of: selectedTab) { _, _ in recordHostedAccountInteraction() }
        .simultaneousGesture(
            DragGesture(minimumDistance: 0).onEnded { _ in
                guard scenePhase == .active else { return }
                recordHostedAccountInteraction()
            }
        )
        .safeAreaInset(edge: .top, spacing: 0) {
            if model.serverReachability == .unavailable {
                ServerOfflineBanner(colors: colors)
            }
        }
    }

    private var syncTab: some View {
        NavigationStack {
            Form {
                Section { NativeHero(title: String(localized: "peppy.toggle_ios_sync_hero_title", table: "Peppy"), detail: String(localized: "peppy.help_ios_contacts_footer", table: "Peppy")) }
                    .listRowInsets(EdgeInsets()).listRowBackground(Color.clear).accessibilityIdentifier("sync-hero")
                ContactsSection(model: model)
                Section {
                    NavigationLink { syncDetails } label: { Text("peppy.toggle_ios_sync_details", tableName: "Peppy") }
                        .accessibilityIdentifier("sync-details-link")
                    if model.activity == .syncing { ProgressView("Syncing…") }
                    if let pending = model.lastSync?.carrierCommandsNotExecuted, pending > 0 {
                        Text("\(pending) send request(s) need attention.")
                            .foregroundStyle(colors.WarningText).accessibilityIdentifier("carrier-pending-summary")
                    }
                } header: {
                    Text("peppy.toggle_ios_sync_status", tableName: "Peppy")
                }.accessibilityIdentifier("sync-status-section")
                Section {
                    LabeledContent("SMS mirroring", value: "Unavailable on iOS")
                        .accessibilityValue("Unavailable").accessibilityIdentifier("sms-mirroring-unavailable-row")
                    LabeledContent("MMS capture", value: "Unavailable on iOS")
                        .accessibilityValue("Unavailable").accessibilityIdentifier("mms-unavailable-row")
                    HStack {
                        LabeledContent("Notification mirroring", value: "Unavailable on iOS")
                            .accessibilityValue("Unavailable").accessibilityIdentifier("mirroring-ios-unavailable-row")
                        InfoButton(topic: String(localized: "peppy.help_ios_mirroring_title", table: "Peppy"), title: String(localized: "peppy.help_ios_mirroring_title", table: "Peppy"), body: String(localized: "peppy.help_ios_mirroring_unavailable", table: "Peppy"), id: "info-mirroring-ios")
                    }
                } header: {
                    HStack {
                        Text("peppy.toggle_ios_unavailable_title", tableName: "Peppy")
                        InfoButton(topic: String(localized: "peppy.help_ios_sms_title", table: "Peppy"), title: String(localized: "peppy.help_ios_sms_title", table: "Peppy"), body: String(localized: "peppy.help_ios_sms_unavailable", table: "Peppy"), id: "info-sms-ios")
                        Spacer()
                    }
                }.accessibilityIdentifier("ios-unavailable-section")
                if model.status.identity?.origin == "https://peppy.pro" { relaySection }
                Link(destination: URL(string: "https://peppy.pro/privacy")!) { Text("peppy.privacy_policy", tableName: "Peppy") }
                    .accessibilityIdentifier("sync-privacy-policy")
                Text("peppy.privacy_policy_scope", tableName: "Peppy").font(.footnote)
            }.formStyle(.grouped).navigationTitle(Text("peppy.toggle_ios_sync_tab", tableName: "Peppy"))
        }.accessibilityIdentifier("sync-tab-screen")
    }

    private var syncDetails: some View {
        Form {
            syncSection
            Section {
                LabeledContent("iOS carrier messaging", value: "Unavailable")
                    .accessibilityValue("Unavailable").accessibilityIdentifier("carrier-status-row")
                ForEach(Array(model.telephony.blockers.enumerated()), id: \.offset) { index, blocker in
                    Text(blocker.explanation).accessibilityIdentifier("carrier-blocker-\(index)")
                }
                if let pending = model.lastSync?.carrierCommandsNotExecuted, pending > 0 {
                    Text("\(pending) send request(s) for this device are queued and will not be sent from iOS.")
                        .accessibilityIdentifier("carrier-pending-commands")
                }
            } header: {
                HStack {
                    Text("Carrier messaging")
                    InfoButton(topic: String(localized: "peppy.help_ios_carrier_title", table: "Peppy"), title: String(localized: "peppy.help_ios_carrier_title", table: "Peppy"), body: String(localized: "peppy.help_ios_carrier_unavailable", table: "Peppy"), id: "info-carrier-ios")
                    Spacer()
                }
            }.accessibilityIdentifier("carrier-section")
        }
        .navigationTitle(Text("peppy.toggle_ios_sync_details", tableName: "Peppy"))
        .accessibilityIdentifier("sync-details-screen")
    }

    private var syncSection: some View {
        Section("Sync") {
            if model.activity == .syncing { ProgressView("Syncing…") }
            if let report = model.lastSync, let date = model.lastSyncDate {
                LabeledContent("Last pass", value: date.formatted(date: .omitted, time: .standard))
                LabeledContent("Sent / received", value: "\(report.uploaded) / \(report.journaled)")
                LabeledContent("Applied", value: "\(report.applied)")
                if report.waitingForKeys > 0 { LabeledContent("Waiting for keys", value: "\(report.waitingForKeys)") }
                if !report.complete { Text("More work remains; it continues on the next foreground pass.") }
            } else { Text("Waiting for the next bounded foreground pass.") }
            Text("peppy.encrypted", tableName: "Peppy").font(.footnote).foregroundStyle(.secondary)
        }.accessibilityIdentifier("sync-section")
    }

    private var accountTab: some View {
        NavigationStack {
            Form {
                Section { NativeHero(title: String(localized: "peppy.toggle_ios_account_hero_title", table: "Peppy"), detail: accountFacts) }
                    .listRowInsets(EdgeInsets()).listRowBackground(Color.clear).accessibilityIdentifier("account-hero")
                NavigationLink { accountDetails } label: { Text("peppy.devices", tableName: "Peppy") }
                    .accessibilityIdentifier("account-devices-link")
                if model.status.role == "owner", model.status.keysUnlocked {
                    Section {
                        Button {
                            Task {
                                ownerPhoneApprovalHost = await model.ownerPhoneApprovalHost()
                                showingOwnerPhoneApproval = ownerPhoneApprovalHost != nil
                            }
                        } label: {
                            Text("peppy.owner_phone_add_cta", tableName: "Peppy").bold().frame(minHeight: 44)
                        }
                        .accessibilityIdentifier("add-phone-button")
                        Button {
                            Task {
                                ownerComputerApprovalHost = await model.ownerComputerApprovalHost()
                                showingOwnerComputerApproval = ownerComputerApprovalHost != nil
                            }
                        } label: {
                            Text("peppy.owner_computer_add_cta", tableName: "Peppy").bold().frame(minHeight: 44)
                        }
                        .accessibilityIdentifier("connect-computer-button")
                    } header: {
                        Text("peppy.owner_phone_section_title", tableName: "Peppy")
                    }
                    .accessibilityIdentifier("owner-phone-section")
                } else if model.status.identity != nil {
                    Section {
                        Label {
                            Text("peppy.owner_computer_unavailable_label", tableName: "Peppy")
                        } icon: {
                            Image(systemName: "laptopcomputer")
                        }
                            .foregroundStyle(colors.TextSecondary)
                            .accessibilityIdentifier("connect-computer-unavailable-row")
                        if model.status.role == "owner" {
                            Text("peppy.owner_computer_unlock_required", tableName: "Peppy")
                                .font(.footnote)
                                .foregroundStyle(colors.TextSecondary)
                                .accessibilityIdentifier("connect-computer-owner-help")
                        } else {
                            Text("peppy.owner_computer_owner_required", tableName: "Peppy")
                                .font(.footnote)
                                .foregroundStyle(colors.TextSecondary)
                                .accessibilityIdentifier("connect-computer-owner-help")
                        }
                    } header: {
                        Text("peppy.owner_computer_section_title", tableName: "Peppy")
                    }
                    .accessibilityIdentifier("owner-computer-unavailable-section")
                }
                NavigationLink { SettingsView(model: model) } label: { Text("peppy.settings", tableName: "Peppy") }
                    .accessibilityIdentifier("account-settings-row")
                Link(destination: URL(string: "https://peppy.pro/privacy")!) {
                    Text("peppy.privacy_policy", tableName: "Peppy")
                }
                .accessibilityIdentifier("account-privacy-policy")
                if let identity = model.status.identity, identity.origin == "https://peppy.pro" {
                    Section {
                        Link(destination: URL(string: "https://peppy.pro/account")!) {
                            Text("peppy.production_manage_account", tableName: "Peppy")
                        }.accessibilityIdentifier("account-management-link")
                    }
                }
                if let error = model.lastError { Text(error).foregroundStyle(colors.Error).accessibilityIdentifier("status-error") }
            }
            .formStyle(.grouped).navigationTitle(Text("peppy.account", tableName: "Peppy"))
        }.accessibilityIdentifier("account-tab-screen")
    }

    private var accountFacts: String {
        guard let identity = model.status.identity else {
            return String(localized: "peppy.toggle_ios_account_facts_unknown", table: "Peppy")
        }
        return "\(identity.origin)\n\(model.status.role ?? String(localized: "peppy.toggle_ios_role_unknown", table: "Peppy")) · \(identity.deviceId)"
    }

    private var accountDetails: some View {
        Form {
            Section("Identity") {
                if let identity = model.status.identity {
                    LabeledContent("Server", value: identity.origin).accessibilityIdentifier("status-server")
                    LabeledContent("Vault", value: identity.vaultId).accessibilityIdentifier("status-vault-id")
                    LabeledContent("Device", value: identity.deviceId).accessibilityIdentifier("status-device-id")
                }
                LabeledContent("Role", value: model.status.role ?? "unknown").accessibilityIdentifier("status-role")
                LabeledContent("Local database", value: model.status.databaseOpen ? "Open (encrypted)" : "Closed").accessibilityIdentifier("status-database")
                LabeledContent("Vault keys", value: model.status.keysUnlocked ? "Unlocked" : "Locked").accessibilityIdentifier("status-keys")
                if let count = model.status.conversations { LabeledContent("Conversations", value: "\(count)").accessibilityIdentifier("status-conversations") }
            }.accessibilityIdentifier("status-section")
            Section("Session") {
                Button("Refresh from server") {
                    recordHostedAccountInteraction()
                    Task { await model.refresh() }
                }
                    .disabled(model.activity != .idle).accessibilityIdentifier("refresh-enrollment-button")
                Button("Disconnect this device…", role: .destructive) { confirmingDisconnect = true }
                    .disabled(model.activity != .idle).accessibilityIdentifier("disconnect-button")
            }
            devicesSection
        }
        .formStyle(.grouped).navigationTitle(Text("peppy.devices", tableName: "Peppy"))
            .confirmationDialog("Disconnect this device?", isPresented: $confirmingDisconnect, titleVisibility: .visible) {
                Button("Disconnect", role: .destructive) { Task { await model.disconnect() } }
                    .accessibilityIdentifier("confirm-disconnect-button")
                Button("Keep connected", role: .cancel) {}
            } message: {
                Text("Disconnecting revokes this device’s server access and stops syncing. Encrypted local data stays archived on this device. To connect again, pair as a new device.")
            }
            .confirmationDialog("Remove this device?", isPresented: Binding(get: { devicePendingRemoval != nil }, set: { if !$0 { devicePendingRemoval = nil } }), titleVisibility: .visible) {
                Button("Remove device", role: .destructive) {
                    if let devicePendingRemoval { Task { await model.revokeDevice(devicePendingRemoval.id) } }
                    devicePendingRemoval = nil
                }.accessibilityIdentifier("confirm-device-remove-button")
                Button(role: .cancel) { devicePendingRemoval = nil } label: { Text("peppy.cancel", tableName: "Peppy") }
            } message: {
                Text("This revokes the selected device's server credential and access.")
            }
        .accessibilityIdentifier("account-devices-screen")
    }

    private var devicesSection: some View {
        Section {
            if !model.devicesLoaded { ProgressView("Loading devices…").accessibilityIdentifier("devices-loading") }
            if model.devicesLoaded && model.devices.isEmpty { Text("No devices found.").accessibilityIdentifier("devices-empty") }
            ForEach(model.devices) { device in
                VStack(alignment: .leading) {
                    Text(device.id + (device.id == model.status.identity?.deviceId ? " (this device)" : ""))
                    Text(device.revoked ? "Revoked" : device.role).font(.footnote).foregroundStyle(.secondary)
                    if device.id == model.status.identity?.deviceId {
                        Button("Sign out", role: .destructive) { confirmingDisconnect = true }
                    } else if model.status.role == "owner" && !device.revoked {
                        Button("Remove device", role: .destructive) { devicePendingRemoval = device }
                            .accessibilityIdentifier("device-remove-\(device.id)")
                    }
                }.accessibilityIdentifier("device-row-\(device.id)")
            }
        } header: {
            Text("peppy.devices", tableName: "Peppy")
        }.accessibilityIdentifier("devices-section").task { await model.refreshDevices() }
    }

    private var relaySection: some View {
        Section {
            Toggle(isOn: Binding(get: { model.backgroundUpdatesEnabled }, set: { model.setBackgroundUpdatesEnabled($0) })) {
                Text("peppy.wake_updates_toggle_label", tableName: "Peppy")
            }
            .accessibilityIdentifier("relay-wake-toggle")
            HStack {
                Text("peppy.wake_status_label", tableName: "Peppy")
                Spacer()
                Text(wakeStatusCopy, tableName: "Peppy")
            }
            .accessibilityIdentifier("relay-wake-status")
            Text(wakeFooterCopy, tableName: "Peppy")
                .font(.footnote).foregroundStyle(.secondary)
                .accessibilityIdentifier("relay-wake-footer")
        } header: {
            Text("peppy.wake_section_title", tableName: "Peppy")
        }.accessibilityIdentifier("relay-health-section")
    }

    private var wakeStatusCopy: LocalizedStringKey {
        switch model.wakeStatus {
        case .registered: "peppy.wake_status_registered"
        case .settingUp: "peppy.wake_status_pending"
        case .retrying: "peppy.wake_status_retrying"
        case .paused: "peppy.wake_status_paused"
        case .unavailable: "peppy.wake_status_unavailable"
        }
    }

    private var wakeFooterCopy: LocalizedStringKey {
        switch model.wakeStatus {
        case .registered: "peppy.wake_footer_registered"
        case .settingUp: "peppy.wake_footer_pending"
        case .retrying: "peppy.wake_footer_retrying"
        case .paused: "peppy.wake_footer_paused"
        case .unavailable: "peppy.wake_footer_unavailable_server"
        }
    }


    /// This is called only from visible controls/navigation, never scene lifecycle or sync work.
    private func recordHostedAccountInteraction() {
        let client = hostedAccountClient ?? (try? HostedAccountClient())
        hostedAccountClient = client
        guard let client else { return }
        Task { try? await client.renewSessionOnUserInteraction() }
    }

    @ViewBuilder private var activityAndError: some View {
        if model.activity != .idle { ProgressView("Verifying with server…").accessibilityIdentifier("enroll-progress") }
        if let error = model.lastError { Text(error).foregroundStyle(colors.Error).accessibilityIdentifier("enroll-error") }
    }
}

struct SettingsView: View {
    @Bindable var model: AppModel
    @Environment(\.colorScheme) private var colorScheme
    @State private var vaultDeleteText = ""
    @State private var deletingVault = false
    @State private var showingVaultDelete = false

    var body: some View {
        Form {
                if model.status.identity?.origin != "https://peppy.pro" {
                    Section("Wake relay") {
                        TextField("Operator relay HTTPS origin", text: $model.relayOrigin)
                            .textInputAutocapitalization(.never).autocorrectionDisabled()
                            .accessibilityIdentifier("relay-origin-field")
                        Button("Enable APNs wake relay") { model.configureRelay() }
                            .disabled(model.relayOrigin.isEmpty || model.status.identity == nil)
                            .accessibilityIdentifier("relay-enable-button")
                    }.accessibilityIdentifier("relay-settings-section")
                }
                Section("About") {
                    LabeledContent("App version", value: "0.1")
                    if let origin = model.status.identity?.origin { LabeledContent("Server", value: origin) }
                    Link(destination: URL(string: "https://peppy.pro/privacy")!) {
                        Text("peppy.privacy_policy", tableName: "Peppy")
                    }
                    .accessibilityIdentifier("settings-privacy-policy")
                    Text("peppy.privacy_policy_scope", tableName: "Peppy")
                        .font(.footnote)
                }
                if model.status.role == "owner" {
                    Section("Danger zone") {
                        Button("Delete vault…", role: .destructive) { vaultDeleteText = ""; showingVaultDelete = true }
                            .accessibilityIdentifier("vault-delete-button")
                        Text("Vault deletion permanently removes server data and device access. Local encrypted data remains until uninstalled.")
                            .font(.footnote).foregroundStyle(.secondary)
                    }
                }
            }.formStyle(.grouped).navigationTitle(Text("peppy.settings", tableName: "Peppy"))
        .accessibilityIdentifier("settings-screen")
            .safeAreaInset(edge: .top, spacing: 0) {
                if model.serverReachability == .unavailable {
                    ServerOfflineBanner(colors: PeppyTokens.colors(for: colorScheme), announcesTransition: false)
                }
            }
            .sheet(isPresented: $showingVaultDelete, onDismiss: {
                vaultDeleteText = ""
                deletingVault = false
            }) {
                NavigationStack { Form {
                    Section("Type ERASE to confirm") {
                        TextField("Type ERASE", text: $vaultDeleteText).autocorrectionDisabled().accessibilityIdentifier("vault-delete-confirm-field")
                        Button("Delete vault", role: .destructive) {
                            guard !deletingVault else { return }
                            deletingVault = true
                            Task {
                                await model.deleteVault()
                                deletingVault = false
                                showingVaultDelete = false
                            }
                        }
                        .disabled(vaultDeleteText != "ERASE" || model.activity != .idle || deletingVault)
                        .accessibilityIdentifier("vault-delete-final-button")
                    }
                }.navigationTitle("Delete vault?").toolbar {
                    ToolbarItem(placement: .topBarLeading) {
                        Button("Cancel") {
                            vaultDeleteText = ""
                            deletingVault = false
                            showingVaultDelete = false
                        }
                    }
                } }
            }
    }
}

private struct NativeHero: View {
    let title: String
    let detail: String

    var body: some View {
        let dark = PeppyTokens.dark
        VStack(alignment: .leading, spacing: 12) {
            HStack(spacing: 18) {
                Image(systemName: "iphone").font(.system(size: 42)).foregroundStyle(dark.Brand)
                Image(systemName: "arrow.right").foregroundStyle(dark.Accent)
                Image(systemName: "laptopcomputer").font(.system(size: 42)).foregroundStyle(dark.Accent)
            }.accessibilityHidden(true)
            Text(title).font(.title2.bold()).foregroundStyle(dark.TextStrong).accessibilityAddTraits(.isHeader)
            Text(detail).foregroundStyle(dark.TextPrimary)
        }.padding(24).frame(maxWidth: .infinity, alignment: .leading).background(dark.SurfaceChrome)
    }
}

struct ServerOfflineBanner: View {
    let colors: PeppyColorScheme
    var announcesTransition = true

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            Image(systemName: "exclamationmark.triangle.fill")
                .foregroundStyle(colors.WarningText)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 2) {
                Text("peppy.server_offline_title", tableName: "Peppy").font(.subheadline.weight(.semibold))
                Text("peppy.server_offline_body", tableName: "Peppy").font(.footnote)
            }
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 16).padding(.vertical, 10)
        .background(colors.WarningSurface)
        .overlay(alignment: .bottom) { colors.WarningBorder.frame(height: 1) }
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("server-offline-banner")
        .onAppear {
            guard announcesTransition else { return }
            #if canImport(UIKit)
            let title = String(localized: "peppy.server_offline_title", table: "Peppy")
            let body = String(localized: "peppy.server_offline_body", table: "Peppy")
            UIAccessibility.post(notification: .announcement, argument: "\(title). \(body)")
            #endif
        }
    }
}
