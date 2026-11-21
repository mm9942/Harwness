package dev.harw.mobile

import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.Color

private val HarwLight = lightColorScheme(
    primary = Color(0xFF864B5E),
    onPrimary = Color.White,
    primaryContainer = Color(0xFFF3DCE3),
    onPrimaryContainer = Color(0xFF4C2535),
    secondary = Color(0xFF584F50),
    secondaryContainer = Color(0xFFEDE3E0),
    onSecondaryContainer = Color(0xFF33292B),
    tertiary = Color(0xFF2E6655),
    tertiaryContainer = Color(0xFFD5ECE2),
    onTertiaryContainer = Color(0xFF123D30),
    background = Color(0xFFF6F2EE),
    onBackground = Color(0xFF2D2829),
    surface = Color(0xFFF6F2EE),
    onSurface = Color(0xFF2D2829),
    surfaceVariant = Color(0xFFF0E6E4),
    onSurfaceVariant = Color(0xFF655858),
    surfaceContainerLowest = Color(0xFFFFFCF9),
    surfaceContainerLow = Color(0xFFF4ECE9),
    surfaceContainer = Color(0xFFF0E7E3),
    surfaceContainerHigh = Color(0xFFEBDEDB),
    outline = Color(0xFF907F80),
    outlineVariant = Color(0xFFD8C8C8),
)

private val HarwDark = darkColorScheme(
    primary = Color(0xFFE9AEBD),
    onPrimary = Color(0xFF4B2232),
    primaryContainer = Color(0xFF66394A),
    onPrimaryContainer = Color(0xFFFFD9E4),
    secondary = Color(0xFFE2C7CB),
    secondaryContainer = Color(0xFF49393C),
    onSecondaryContainer = Color(0xFFFDE4E7),
    tertiary = Color(0xFFA8DAC8),
    tertiaryContainer = Color(0xFF234C40),
    onTertiaryContainer = Color(0xFFC4F0E0),
    background = Color(0xFF201A1C),
    onBackground = Color(0xFFF5E4E7),
    surface = Color(0xFF201A1C),
    onSurface = Color(0xFFF5E4E7),
    surfaceVariant = Color(0xFF44363A),
    onSurfaceVariant = Color(0xFFDECDD0),
    surfaceContainerLowest = Color(0xFF1B1618),
    surfaceContainerLow = Color(0xFF261E21),
    surfaceContainer = Color(0xFF302629),
    surfaceContainerHigh = Color(0xFF3B2F32),
    outline = Color(0xFFAA9599),
    outlineVariant = Color(0xFF554448),
)

@Composable
internal fun HarwTheme(content: @Composable () -> Unit) {
    MaterialTheme(colorScheme = if (isSystemInDarkTheme()) HarwDark else HarwLight, content = content)
}
