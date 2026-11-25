package dev.harw.mobile

import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.ColorScheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.Color

// Keep text accents legible on each background; solid actions use the reference pink.
private val HarwLightAccent = Color(0xFF9B3059)
private val HarwActionPink = Color(0xFFF4ABBE)
private val HarwActionInk = Color(0xFF391224)
private val HarwDarkAccent = HarwActionPink

private val HarwLight = lightColorScheme(
    primary = HarwLightAccent,
    onPrimary = Color.White,
    primaryContainer = Color(0xFFF9DFE9),
    onPrimaryContainer = Color(0xFF542037),
    secondary = Color(0xFF835167),
    onSecondary = Color.White,
    secondaryContainer = Color(0xFFF4E2EA),
    onSecondaryContainer = Color(0xFF4A2839),
    // Tertiary is reserved for healthy connection indicators.
    tertiary = Color(0xFF2E6655),
    onTertiary = Color.White,
    tertiaryContainer = Color(0xFFD5ECE2),
    onTertiaryContainer = Color(0xFF123D30),
    background = Color(0xFFF7F6F7),
    onBackground = Color(0xFF232226),
    surface = Color(0xFFF7F6F7),
    onSurface = Color(0xFF232226),
    surfaceVariant = Color(0xFFE9E6EA),
    onSurfaceVariant = Color(0xFF544D55),
    surfaceContainerLowest = Color.White,
    surfaceContainerLow = Color(0xFFF2F0F2),
    surfaceContainer = Color(0xFFECE9ED),
    surfaceContainerHigh = Color(0xFFE6E2E7),
    surfaceContainerHighest = Color(0xFFE0DCE1),
    surfaceTint = HarwLightAccent,
    inverseSurface = Color(0xFF313034),
    inverseOnSurface = Color(0xFFF3F0F3),
    inversePrimary = HarwDarkAccent,
    outline = Color(0xFF8B7180),
    outlineVariant = Color(0xFFD9CDD3),
)

private val HarwDark = darkColorScheme(
    primary = HarwDarkAccent,
    onPrimary = HarwActionInk,
    primaryContainer = Color(0xFF522B3C),
    onPrimaryContainer = Color(0xFFFFDCE8),
    secondary = Color(0xFFD7A5B9),
    onSecondary = Color(0xFF432634),
    secondaryContainer = Color(0xFF442D38),
    onSecondaryContainer = Color(0xFFFFDEE9),
    // Keep connection health distinct from brand accents.
    tertiary = Color(0xFFA8DAC8),
    onTertiary = Color(0xFF10372D),
    tertiaryContainer = Color(0xFF234C40),
    onTertiaryContainer = Color(0xFFC4F0E0),
    background = Color(0xFF232326),
    onBackground = Color(0xFFF3F0F3),
    surface = Color(0xFF232326),
    onSurface = Color(0xFFF3F0F3),
    surfaceVariant = Color(0xFF39363B),
    onSurfaceVariant = Color(0xFFD0C9D0),
    surfaceContainerLowest = Color(0xFF1D1D20),
    surfaceContainerLow = Color(0xFF26262A),
    surfaceContainer = Color(0xFF2C2C30),
    surfaceContainerHigh = Color(0xFF323237),
    surfaceContainerHighest = Color(0xFF3B3A40),
    surfaceTint = HarwDarkAccent,
    inverseSurface = Color(0xFFE6E2E7),
    inverseOnSurface = Color(0xFF313034),
    inversePrimary = HarwLightAccent,
    outline = Color(0xFFA88D9C),
    outlineVariant = Color(0xFF554953),
)

/** Opaque action fill: floating controls must cover the list underneath. */
internal fun ColorScheme.harwAccentButtonContainer(enabled: Boolean): Color =
    if (enabled) HarwActionPink else surfaceContainerHighest

internal fun ColorScheme.harwAccentButtonContent(enabled: Boolean): Color =
    if (enabled) HarwActionInk else onSurfaceVariant

/**
 * Maps the theme preference from SettingsHub PREF_THEME to a dark-mode override.
 * Values: "system" (or anything else) -> null (follow the system), "light" -> false, "dark" -> true.
 */
internal fun themeDarkOverride(pref: String): Boolean? = when (pref) {
    "light" -> false
    "dark" -> true
    else -> null
}

@Composable
internal fun HarwTheme(dark: Boolean? = null, content: @Composable () -> Unit) {
    MaterialTheme(colorScheme = if (dark ?: isSystemInDarkTheme()) HarwDark else HarwLight, content = content)
}
