package dev.harw.mobile

import java.util.Locale
import org.junit.Assert.assertEquals
import org.junit.Assert.assertSame
import org.junit.Test

class HarwUiLanguageTest {
    @Test fun missingOrDamagedPreferenceFollowsSupportedPrimaryDeviceLanguage() {
        val device = Locale.forLanguageTag("de-AT")
        for (missingOrInvalid in listOf(null, "", "system", "it", 3, "de_DE")) {
            val selection = HarwUiLanguagePreference.restored(missingOrInvalid)
            assertSame(HarwUiLanguagePreference.FollowDevice, selection)
            assertSame(HarwUiLanguage.German, selection.resolve(device))
        }
    }

    @Test fun unsupportedOrMissingDeviceLanguageFallsBackToEnglish() {
        assertSame(HarwUiLanguage.English, HarwUiLanguagePreference.FollowDevice.resolve(Locale.ITALIAN))
        assertSame(HarwUiLanguage.English, HarwUiLanguagePreference.FollowDevice.resolve(null))
    }

    @Test fun explicitChoiceIsNotCollapsedIntoFollowingTheDevice() {
        val explicitEnglish = HarwUiLanguagePreference.restored("en")
        assertSame(HarwUiLanguage.English, explicitEnglish.resolve(Locale.ENGLISH))
        assertSame(HarwUiLanguage.English, explicitEnglish.resolve(Locale.GERMAN))
        val explicitGerman = HarwUiLanguagePreference.restored("de")
        assertSame(HarwUiLanguage.German, explicitGerman.resolve(Locale.ENGLISH))
        assertSame(HarwUiLanguage.German, HarwUiLanguagePreference.FollowDevice.resolve(Locale.GERMAN))
    }

    @Test fun allFourShippedSelectionsRoundTripByStableTag() {
        val expected = listOf("en", "de", "fr", "es")
        assertEquals(expected, HarwUiLanguage.entries.map { it.code })
        HarwUiLanguage.entries.forEach { language ->
            assertEquals(HarwUiLanguagePreference.Explicit(language), HarwUiLanguagePreference.restored(language.code))
        }
    }

    @Test fun localizedResourceLookupKeepsEnglishAsTheOnlyFallback() {
        assertEquals(listOf("en"), HarwUiLanguage.English.resourceLocales.map { it.language })
        assertEquals(listOf("de", "en"), HarwUiLanguage.German.resourceLocales.map { it.language })
    }
}
