package dev.peppy.mobile.restrictedsetup

import androidx.compose.runtime.Composable

@Composable
internal fun RestrictedSetupEntry(onAccessChanged: () -> Unit) = Unit

internal fun restrictedSetupAvailable(context: android.content.Context): Boolean = false
