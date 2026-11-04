package dev.peppy.mobile.restrictedsetup

import kotlinx.coroutines.flow.Flow

internal enum class AdbServiceKind { PAIRING, CONNECT }

internal interface AdbServiceDiscovery {
    fun ports(kind: AdbServiceKind): Flow<Int?>
}
