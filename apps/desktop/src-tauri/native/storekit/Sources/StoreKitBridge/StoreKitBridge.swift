import AppKit
import Foundation
import StoreKit

public typealias Completion = @convention(c) (UnsafeMutableRawPointer?, UnsafePointer<CChar>?) -> Void

private final class Callback: @unchecked Sendable {
    let context: UnsafeMutableRawPointer?
    let completion: Completion?

    init(_ context: UnsafeMutableRawPointer?, _ completion: Completion?) {
        self.context = context
        self.completion = completion
    }
}

private let observerLock = NSLock()
nonisolated(unsafe) private var nextObserverHandle: Int64 = 1
nonisolated(unsafe) private var transactionObservers: [Int64: Task<Void, Never>] = [:]

private func retainObserver(_ task: Task<Void, Never>, in observers: inout [Int64: Task<Void, Never>]) -> Int64 {
    observerLock.lock()
    defer { observerLock.unlock() }
    let handle = nextObserverHandle
    nextObserverHandle += 1
    observers[handle] = task
    return handle
}

private func decode(_ input: UnsafePointer<CChar>?) -> [String: Any] {
    guard let input, let data = String(validatingCString: input)?.data(using: .utf8),
          let value = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { return [:] }
    return value
}

private func encode(_ value: Any) -> String {
    guard JSONSerialization.isValidJSONObject(value),
          let data = try? JSONSerialization.data(withJSONObject: value),
          let json = String(data: data, encoding: .utf8) else {
        return #"{"error":"storekit-bridge"}"#
    }
    return json
}

private func respond(_ callback: Callback, _ value: Any) {
    let json = encode(value)
    json.withCString { callback.completion?(callback.context, $0) }
}

@available(macOS 12, *)
private func transactionEntry(_ result: VerificationResult<Transaction>) -> [String: String]? {
    guard case .verified(let transaction) = result else { return nil }
    return ["signedTransaction": result.jwsRepresentation, "transactionId": String(transaction.id)]
}

@_cdecl("storekit_products")
public func storekitProducts(_ request: UnsafePointer<CChar>?, _ context: UnsafeMutableRawPointer?, _ completion: Completion?) {
    let identifiers = decode(request)["ids"] as? [String] ?? []
    let callback = Callback(context, completion)
    Task {
        guard #available(macOS 12, *) else {
            respond(callback, ["error": "unsupported"])
            return
        }
        do {
            let products = try await Product.products(for: identifiers).map { product in
                [
                    "id": product.id,
                    "displayName": product.displayName,
                    "displayPrice": product.displayPrice,
                    "period": product.subscription.map { subscription in
                        "\(subscription.subscriptionPeriod.value) \(subscription.subscriptionPeriod.unit)"
                    } ?? NSNull(),
                    "kind": product.type == .autoRenewable ? "plan" : "storage",
                ]
            }
            respond(callback, products)
        } catch {
            respond(callback, ["error": "products-unavailable"])
        }
    }
}

@_cdecl("storekit_purchase")
public func storekitPurchase(_ request: UnsafePointer<CChar>?, _ context: UnsafeMutableRawPointer?, _ completion: Completion?) {
    let values = decode(request)
    let productID = values["productId"] as? String ?? ""
    let token = (values["appAccountToken"] as? String).flatMap(UUID.init(uuidString:))
    let callback = Callback(context, completion)
    Task { @MainActor in
        guard #available(macOS 12, *) else {
            respond(callback, ["result": "unverified"])
            return
        }
        guard let product = try? await Product.products(for: [productID]).first, let token else {
            respond(callback, ["result": "unverified"])
            return
        }
        do {
            let options: Set<Product.PurchaseOption> = [.appAccountToken(token)]
            let purchase: Product.PurchaseResult
            if #available(macOS 15.2, *), let window = NSApp.keyWindow ?? NSApp.mainWindow {
                purchase = try await product.purchase(confirmIn: window, options: options)
            } else {
                purchase = try await product.purchase(options: options)
            }
            switch purchase {
            case .success(let verification):
                guard case .verified(let transaction) = verification else {
                    respond(callback, ["result": "unverified"])
                    return
                }
                respond(callback, ["result": "purchased", "signedTransaction": verification.jwsRepresentation, "transactionId": String(transaction.id)])
            case .pending:
                respond(callback, ["result": "pending"])
            case .userCancelled:
                respond(callback, ["result": "cancelled"])
            @unknown default:
                respond(callback, ["result": "unverified"])
            }
        } catch {
            respond(callback, ["result": "unverified"])
        }
    }
}

@_cdecl("storekit_finish")
public func storekitFinish(_ request: UnsafePointer<CChar>?, _ context: UnsafeMutableRawPointer?, _ completion: Completion?) {
    let identifier = decode(request)["transactionId"] as? String
    let callback = Callback(context, completion)
    Task {
        guard #available(macOS 12, *), let identifier else { respond(callback, ["finished": false]); return }
        for await result in Transaction.unfinished {
            if case .verified(let transaction) = result, String(transaction.id) == identifier {
                await transaction.finish()
                respond(callback, ["finished": true])
                return
            }
        }
        for await result in Transaction.currentEntitlements {
            if case .verified(let transaction) = result, String(transaction.id) == identifier {
                respond(callback, ["finished": true])
                return
            }
        }
        respond(callback, ["finished": false])
    }
}

@available(macOS 12, *)
private func transactions(_ stream: Transaction.Transactions) async -> [[String: String]] {
    var values: [[String: String]] = []
    for await result in stream {
        if let entry = transactionEntry(result) { values.append(entry) }
    }
    return values
}

@_cdecl("storekit_unfinished")
public func storekitUnfinished(_ context: UnsafeMutableRawPointer?, _ completion: Completion?) {
    let callback = Callback(context, completion)
    Task { guard #available(macOS 12, *) else { respond(callback, ["error": "unsupported"]); return }; respond(callback, await transactions(Transaction.unfinished)) }
}

@_cdecl("storekit_current_entitlements")
public func storekitCurrentEntitlements(_ context: UnsafeMutableRawPointer?, _ completion: Completion?) {
    let callback = Callback(context, completion)
    Task { guard #available(macOS 12, *) else { respond(callback, ["error": "unsupported"]); return }; respond(callback, await transactions(Transaction.currentEntitlements)) }
}

@_cdecl("storekit_sync")
public func storekitSync(_ context: UnsafeMutableRawPointer?, _ completion: Completion?) {
    let callback = Callback(context, completion)
    Task { guard #available(macOS 12, *) else { respond(callback, ["error": "unsupported"]); return }; do { try await AppStore.sync(); respond(callback, ["synced": true]) } catch { respond(callback, ["synced": false]) } }
}

@_cdecl("storekit_storefront_country")
public func storekitStorefrontCountry(_ context: UnsafeMutableRawPointer?, _ completion: Completion?) {
    let callback = Callback(context, completion)
    Task { guard #available(macOS 12, *) else { respond(callback, ["country": NSNull()]); return }; respond(callback, ["country": await Storefront.current?.countryCode as Any]) }
}

@_cdecl("storekit_observe_transaction_updates")
public func storekitObserveTransactionUpdates(_ context: UnsafeMutableRawPointer?, _ callback: Completion?) -> Int64 {
    guard #available(macOS 12, *) else { return 0 }
    let callback = Callback(context, callback)
    let task = Task {
        for await result in Transaction.updates {
            guard !Task.isCancelled else { return }
            if let entry = transactionEntry(result) { respond(callback, entry) }
        }
    }
    return retainObserver(task, in: &transactionObservers)
}
