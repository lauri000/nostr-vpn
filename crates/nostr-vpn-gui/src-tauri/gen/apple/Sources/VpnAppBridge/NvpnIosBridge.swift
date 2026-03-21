import Foundation
import NetworkExtension

private let vpnBridgeQueueKey = DispatchSpecificKey<Void>()
private let vpnBridgeQueue: DispatchQueue = {
    let queue = DispatchQueue(label: "to.iris.nvpn.bridge", qos: .userInitiated)
    queue.setSpecific(key: vpnBridgeQueueKey, value: ())
    return queue
}()

private func runBridgeTask<T>(_ work: @escaping () throws -> T) throws -> T {
    if DispatchQueue.getSpecific(key: vpnBridgeQueueKey) != nil {
        return try work()
    }

    let semaphore = DispatchSemaphore(value: 0)
    var result: Result<T, Error>!
    vpnBridgeQueue.async {
        result = Result { try work() }
        semaphore.signal()
    }
    semaphore.wait()
    return try result.get()
}

private func waitForAsyncResult<T>(
    operation: String,
    timeout: TimeInterval = 10,
    _ work: (@escaping (Result<T, Error>) -> Void) -> Void
) throws -> T {
    let semaphore = DispatchSemaphore(value: 0)
    var result: Result<T, Error>?
    work { completionResult in
        result = completionResult
        semaphore.signal()
    }

    if semaphore.wait(timeout: .now() + timeout) == .timedOut {
        throw VpnSharedError.operationTimedOut(operation)
    }

    guard let result else {
        throw VpnSharedError.managerUnavailable
    }

    return try result.get()
}

private func loadAllManagers() throws -> [NETunnelProviderManager] {
    try waitForAsyncResult(operation: "VPN manager preferences to load") { completion in
        NETunnelProviderManager.loadAllFromPreferences { managers, error in
            if let error {
                completion(.failure(error))
                return
            }
            completion(.success(managers ?? []))
        }
    }
}

private func saveManager(_ manager: NETunnelProviderManager) throws {
    let _: Void = try waitForAsyncResult(operation: "VPN manager preferences to save") { completion in
        manager.saveToPreferences { error in
            if let error {
                completion(.failure(error))
                return
            }
            completion(.success(()))
        }
    }
}

private func reloadManager(_ manager: NETunnelProviderManager) throws {
    let _: Void = try waitForAsyncResult(operation: "VPN manager preferences to reload") { completion in
        manager.loadFromPreferences { error in
            if let error {
                completion(.failure(error))
                return
            }
            completion(.success(()))
        }
    }
}

private func loadOrCreateManager() throws -> NETunnelProviderManager {
    if let existing = try loadAllManagers().first(where: {
        ($0.protocolConfiguration as? NETunnelProviderProtocol)?.providerBundleIdentifier
            == vpnPacketTunnelBundleIdentifier
    }) {
        return existing
    }

    let manager = NETunnelProviderManager()
    let configuration = NETunnelProviderProtocol()
    configuration.providerBundleIdentifier = vpnPacketTunnelBundleIdentifier
    configuration.serverAddress = "Nostr VPN"
    manager.protocolConfiguration = configuration
    manager.localizedDescription = "Nostr VPN"
    manager.isEnabled = true
    return manager
}

private func configureManager(_ manager: NETunnelProviderManager, request: NvpnStartRequest?) {
    let configuration =
        (manager.protocolConfiguration as? NETunnelProviderProtocol) ?? NETunnelProviderProtocol()
    configuration.providerBundleIdentifier = vpnPacketTunnelBundleIdentifier
    configuration.serverAddress = request?.sessionName ?? "Nostr VPN"

    if let request {
        configuration.providerConfiguration = [
            "sessionName": request.sessionName,
            "configJson": request.configJson,
            "localAddress": request.localAddress,
            "dnsServers": request.dnsServers,
            "searchDomains": request.searchDomains,
            "mtu": Int(request.mtu),
        ]
    }

    manager.protocolConfiguration = configuration
    manager.localizedDescription = "Nostr VPN"
    manager.isEnabled = true
}

private func currentBridgeStatus(for manager: NETunnelProviderManager) -> NvpnBridgeStatus {
    let connectionStatus = manager.connection.status
    let active = tunnelConnectionIsActive(connectionStatus)
    var stateJson: String?
    var error = recordedTunnelError()

    if active,
       let session = manager.connection as? NETunnelProviderSession,
       let providerStatus = try? requestProviderStatus(session)
    {
        stateJson = providerStatus.stateJson
        if let providerError = providerStatus.error, !providerError.isEmpty {
            error = providerError
        }
    }

    let prepared =
        (manager.protocolConfiguration as? NETunnelProviderProtocol)?.providerBundleIdentifier
        == vpnPacketTunnelBundleIdentifier
    return NvpnBridgeStatus(prepared: prepared, active: active, error: error, stateJson: stateJson)
}

private func requestProviderStatus(_ session: NETunnelProviderSession) throws
    -> PacketTunnelBridgeStatus
{
    let responseData: Data = try waitForAsyncResult(
        operation: "the packet tunnel provider status response"
    ) { completion in
        let command = Data("status".utf8)
        do {
            try session.sendProviderMessage(command) { response in
                guard let response else {
                    completion(.failure(VpnSharedError.managerUnavailable))
                    return
                }
                completion(.success(response))
            }
        } catch {
            completion(.failure(error))
        }
    }

    guard let responseText = String(data: responseData, encoding: .utf8),
          let decoded = try? JSONDecoder().decode(PacketTunnelBridgeStatus.self, from: responseData)
    else {
        throw VpnSharedError.managerUnavailable
    }

    if decoded.stateJson == nil && decoded.error == nil && responseText.isEmpty {
        throw VpnSharedError.managerUnavailable
    }

    return decoded
}

private func stopTunnelIfNeeded(_ manager: NETunnelProviderManager) {
    guard tunnelConnectionIsActive(manager.connection.status) else {
        return
    }
    manager.connection.stopVPNTunnel()

    let deadline = Date().addingTimeInterval(5)
    while tunnelConnectionIsActive(manager.connection.status) && Date() < deadline {
        Thread.sleep(forTimeInterval: 0.1)
    }
}

@_cdecl("nvpn_ios_prepare")
public func nvpn_ios_prepare() -> UnsafeMutablePointer<CChar>? {
    do {
        let status = try runBridgeTask {
            let manager = try loadOrCreateManager()
            configureManager(manager, request: try? loadStoredStartRequest())
            try saveManager(manager)
            try reloadManager(manager)
            updateRecordedTunnelError(nil)
            return currentBridgeStatus(for: manager)
        }
        return makeStatusCString(
            prepared: true,
            active: status.active,
            error: status.error,
            stateJson: status.stateJson
        )
    } catch {
        updateRecordedTunnelError(error.localizedDescription)
        return makeStatusCString(
            prepared: false,
            active: false,
            error: error.localizedDescription,
            stateJson: nil
        )
    }
}

@_cdecl("nvpn_ios_start")
public func nvpn_ios_start(_ requestPointer: UnsafePointer<CChar>?) -> UnsafeMutablePointer<CChar>? {
    do {
        let request = try decodeStartRequest(requestPointer)
        let status = try runBridgeTask {
            try storeStartRequest(request)
            updateRecordedTunnelError(nil)

            let manager = try loadOrCreateManager()
            configureManager(manager, request: request)
            try saveManager(manager)
            try reloadManager(manager)
            stopTunnelIfNeeded(manager)

            guard let session = manager.connection as? NETunnelProviderSession else {
                throw VpnSharedError.managerUnavailable
            }
            try session.startTunnel(options: nil)

            return currentBridgeStatus(for: manager)
        }
        return makeStatusCString(
            prepared: true,
            active: status.active,
            error: status.error,
            stateJson: status.stateJson
        )
    } catch {
        updateRecordedTunnelError(error.localizedDescription)
        return makeStatusCString(
            prepared: false,
            active: false,
            error: error.localizedDescription,
            stateJson: nil
        )
    }
}

@_cdecl("nvpn_ios_stop")
public func nvpn_ios_stop() -> UnsafeMutablePointer<CChar>? {
    do {
        let active = try runBridgeTask {
            let manager = try loadOrCreateManager()
            stopTunnelIfNeeded(manager)
            updateRecordedTunnelError(nil)
            return tunnelConnectionIsActive(manager.connection.status)
        }
        return makeStatusCString(
            prepared: true,
            active: active,
            error: nil,
            stateJson: nil
        )
    } catch {
        updateRecordedTunnelError(error.localizedDescription)
        return makeStatusCString(
            prepared: false,
            active: false,
            error: error.localizedDescription,
            stateJson: nil
        )
    }
}

@_cdecl("nvpn_ios_status")
public func nvpn_ios_status() -> UnsafeMutablePointer<CChar>? {
    do {
        let status = try runBridgeTask {
            let manager = try loadOrCreateManager()
            return currentBridgeStatus(for: manager)
        }
        return makeStatusCString(
            prepared: status.prepared,
            active: status.active,
            error: status.error,
            stateJson: status.stateJson
        )
    } catch {
        return makeStatusCString(
            prepared: false,
            active: false,
            error: error.localizedDescription,
            stateJson: nil
        )
    }
}

@_cdecl("nvpn_ios_free_string")
public func nvpn_ios_free_string(_ pointer: UnsafeMutablePointer<CChar>?) {
    guard let pointer else {
        return
    }
    free(pointer)
}
