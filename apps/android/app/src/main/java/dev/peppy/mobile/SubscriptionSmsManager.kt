package dev.peppy.mobile

import android.content.Context
import android.os.Build
import android.telephony.SmsManager

internal fun subscriptionSmsManager(context: Context, subscriptionId: Int): SmsManager =
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
        context.getSystemService(SmsManager::class.java).createForSubscriptionId(subscriptionId)
    } else {
        @Suppress("DEPRECATION")
        SmsManager.getSmsManagerForSubscriptionId(subscriptionId)
    }
