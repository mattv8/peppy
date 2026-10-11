import SwiftUI

@MainActor
struct PhoneEnrollmentView: View {
    let model: AppModel
    @Binding var isPresented: Bool
    @State private var status: PhoneEnrollmentStatus?
    @State private var sharedSecret = ""
    @State private var codesMatch = false

    var body: some View {
        NavigationStack {
            VStack(spacing: 16) {
                QRScannerView { qr in Task { status = await model.beginPhoneEnrollment(qr: qr) } }
                    .frame(height: 300).clipShape(RoundedRectangle(cornerRadius: 16))
                    .accessibilityIdentifier("phone-enrollment-scanner")
                if let status {
                    if let code = status.verificationCode {
                        Text(code).font(.title.monospaced())
                            .accessibilityIdentifier("phone-enrollment-sas")
                        Text("Compare this code with the owner before continuing.")
                    }
                    if status.awaitingOwner {
                        Text("Waiting for the owner to approve this phone.").accessibilityIdentifier("phone-enrollment-waiting")
                        ProgressView()
                    } else {
                        Toggle("The codes match on both devices.", isOn: $codesMatch)
                            .accessibilityIdentifier("phone-enrollment-sas-confirm")
                        if status.requiresSharedSecret {
                            SecureField("Vault shared secret", text: $sharedSecret)
                                .textContentType(.password).accessibilityIdentifier("phone-enrollment-shared-secret")
                        }
                        Button("Confirm connection") {
                            let secret = status.requiresSharedSecret ? sharedSecret : nil
                            sharedSecret = ""
                            Task { await model.confirmPhoneEnrollment(sharedSecret: secret); if model.status.identity != nil { isPresented = false } }
                        }
                        .disabled(!codesMatch || (status.requiresSharedSecret && sharedSecret.isEmpty))
                        .accessibilityIdentifier("phone-enrollment-confirm")
                    }
                } else {
                    Text("Scan the QR code from your Peppy owner device.")
                }
                if let error = model.lastError {
                    Text(error).foregroundStyle(.red).accessibilityIdentifier("phone-enrollment-error")
                    Button("Scan again") { status = nil; codesMatch = false }
                        .accessibilityIdentifier("phone-enrollment-retry")
                }
            }
            .padding().navigationTitle("Connect phone")
            .toolbar { ToolbarItem(placement: .topBarLeading) { Button("Cancel") { model.cancelPhoneEnrollment(); isPresented = false } } }
        }
        .accessibilityIdentifier("phone-enrollment-screen")
        .task(id: status?.awaitingOwner) {
            guard status?.awaitingOwner == true else { return }
            while !Task.isCancelled, status?.awaitingOwner == true {
                try? await Task.sleep(for: .seconds(3))
                if let next = await model.phoneEnrollmentStatus() { status = next } else { return }
            }
        }
    }
}
