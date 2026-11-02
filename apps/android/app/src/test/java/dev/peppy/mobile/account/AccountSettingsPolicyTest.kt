package dev.peppy.mobile.account

import org.junit.Assert.*
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test


class AccountSettingsPolicyTest {
    @Test fun addPhoneRequiresOwnerRoleAndHostEligibility() {
        assertTrue(AccountSettingsPolicy.canAddPhone("owner", true))
        assertFalse(AccountSettingsPolicy.canAddPhone("gateway", true))
        assertFalse(AccountSettingsPolicy.canAddPhone("owner", false))
    }
}
