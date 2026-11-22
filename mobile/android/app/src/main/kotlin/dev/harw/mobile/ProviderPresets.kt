package dev.harw.mobile

import java.net.URI
import java.util.Locale
import uniffi.harw_mobile_ffi.ProviderKind

/**
 * Anbieter-Vorlagen der alten Kit-App (ProviderCatalog, 24 Zeilen), als Daten portiert.
 * Der Kern kennt nur fuenf Familien ([ProviderKind]); [kind] bildet das Protokoll darauf ab.
 */
internal data class ProviderPreset(
    val id: String,
    val label: String,
    val api: String,
    val endpoint: String,
    val model: String,
    val modelHint: String,
    val endpointHint: String,
    val description: String,
    val available: Boolean,
    val unavailableReason: String,
    val credentialLabel: String,
) {
    /** Familie im Kern; `null`, wenn der Kern das Protokoll (noch) nicht spricht. */
    val kind: ProviderKind?
        get() = when (api) {
            "anthropic-messages" -> ProviderKind.ANTHROPIC
            "openai-responses" -> ProviderKind.OPEN_AI
            "openai-chat", "bedrock-chat", "vertex-openai" -> ProviderKind.OPEN_AI_COMPATIBLE
            else -> null
        }

    /** Warum die Vorlage in dieser App nicht speicherbar ist, sonst `null`. */
    val blocker: String?
        get() = when {
            !available -> unavailableReason
            kind == null -> "Das Protokoll $api wird vom Kern auf diesem Gerät noch nicht unterstützt (api-key-Header oder Responses-Variante fehlt)."
            else -> null
        }

    val group: String
        get() = when {
            id in GATEWAY_IDS -> "Gateways"
            id.startsWith("custom-") -> "Eigene"
            id.startsWith("azure-") || id.startsWith("bedrock-") || id.startsWith("vertex-") -> "Cloud"
            else -> "Direkt"
        }
}

private val GATEWAY_IDS = setOf("openrouter", "vercel-ai-gateway", "huggingface")

internal val PROVIDER_GROUPS = listOf("Alle", "Gateways", "Cloud", "Direkt", "Eigene")

internal val PROVIDER_PRESETS: List<ProviderPreset> = listOf(
    ProviderPreset("openai", "OpenAI", "openai-responses", "https://api.openai.com/v1", "gpt-4.1-mini",
        "Publisher-Modell-ID, zum Beispiel gpt-4.1-mini.",
        "OpenAI-API-Stammadresse mit /v1 am Ende.",
        "OpenAI-Publisher-Modelle über die Responses-API.",
        true, "", "API-Schlüssel"),
    ProviderPreset("anthropic", "Anthropic", "anthropic-messages", "https://api.anthropic.com/v1", "claude-sonnet-4-5",
        "Anthropic-Publisher-Modell-ID.",
        "Anthropic-API-Stammadresse mit /v1 am Ende.",
        "Anthropic-Publisher-Modelle über die Messages-API.",
        true, "", "API-Schlüssel"),
    ProviderPreset("google-gemini", "Google Gemini", "openai-chat", "https://generativelanguage.googleapis.com/v1beta/openai", "gemini-2.5-flash",
        "Publisher-Modell-ID, zum Beispiel gemini-2.5-flash.",
        "OpenAI-kompatible Gemini-Endpunkt-Stammadresse.",
        "Google-Publisher-Modelle auf Gemini über die OpenAI-kompatible Chat-Completions-API.",
        true, "", "API-Schlüssel"),
    ProviderPreset("xai", "xAI", "openai-chat", "https://api.x.ai/v1", "grok-3-mini",
        "Publisher-Modell-ID.",
        "xAI-API-Stammadresse mit /v1 am Ende.",
        "xAI-Publisher-Modelle über die OpenAI-kompatible Chat-Completions-API.",
        true, "", "API-Schlüssel"),
    ProviderPreset("deepseek", "DeepSeek", "openai-chat", "https://api.deepseek.com/v1", "deepseek-chat",
        "Publisher-Modell-ID.",
        "DeepSeek-API-Stammadresse mit /v1 am Ende.",
        "DeepSeek-Publisher-Modelle über die OpenAI-kompatible Chat-Completions-API.",
        true, "", "API-Schlüssel"),
    ProviderPreset("mistral", "Mistral", "openai-chat", "https://api.mistral.ai/v1", "mistral-small-latest",
        "Publisher-Modell-ID.",
        "Mistral-API-Stammadresse mit /v1 am Ende.",
        "Mistral-Publisher-Modelle über die OpenAI-kompatible Chat-Completions-API.",
        true, "", "API-Schlüssel"),
    ProviderPreset("groq", "Groq", "openai-chat", "https://api.groq.com/openai/v1", "llama-3.3-70b-versatile",
        "In Ihrem Groq-Konto freigegebene Modell-ID; Publisher und Hosting-Plattform können verschieden sein.",
        "OpenAI-kompatible Groq-API-Stammadresse.",
        "Von Groq gehostete Modelle über die OpenAI-kompatible Chat-Completions-API.",
        true, "", "API-Schlüssel"),
    ProviderPreset("together", "Together AI", "openai-chat", "https://api.together.xyz/v1", "meta-llama/Meta-Llama-3.1-8B-Instruct-Turbo",
        "Bei Together freigegebene Modell-ID; Publisher und Hosting-Plattform können verschieden sein.",
        "Together-API-Stammadresse mit /v1 am Ende.",
        "Publisher-Modelle auf Together über die OpenAI-kompatible Chat-Completions-API.",
        true, "", "API-Schlüssel"),
    ProviderPreset("fireworks", "Fireworks AI", "openai-chat", "https://api.fireworks.ai/inference/v1", "accounts/fireworks/models/llama-v3p1-8b-instruct",
        "In Ihrem Fireworks-Konto verfügbare Modell-ID; Publisher und Hosting-Plattform können verschieden sein.",
        "Fireworks-Inference-API-Stammadresse.",
        "Auf Fireworks gehostete Modelle über die OpenAI-kompatible Chat-Completions-API.",
        true, "", "API-Schlüssel"),
    ProviderPreset("openrouter", "OpenRouter", "openai-chat", "https://openrouter.ai/api/v1", "openai/gpt-4.1-mini",
        "OpenRouter-Modellkennung mit Publisher im Namen.",
        "OpenRouter-API-Stammadresse.",
        "Von OpenRouter vermittelte Publisher-Modelle über die OpenAI-kompatible Chat-Completions-API.",
        true, "", "API-Schlüssel"),
    ProviderPreset("vercel-ai-gateway", "Vercel AI Gateway", "openai-chat", "https://ai-gateway.vercel.sh/v1", "openai/gpt-4.1-mini",
        "Gateway-Modell-ID, meist publisher/model; der Publisher ist nicht das Gateway.",
        "Vercel-AI-Gateway-API-Stammadresse.",
        "Von Vercel AI Gateway vermittelte Publisher-Modelle über die OpenAI-kompatible Chat-Completions-API.",
        true, "", "API-Schlüssel"),
    ProviderPreset("huggingface", "Hugging Face", "openai-chat", "https://router.huggingface.co/v1", "openai/gpt-oss-120b:fastest",
        "Publisher/Modell-ID; die Publisher-Modelle werden über Hugging Face Inference Providers vermittelt.",
        "Exakte Hugging-Face-Router-Adresse https://router.huggingface.co/v1.",
        "Leitet Modelle verschiedener Publisher über Hugging Face Inference Providers weiter. Erfordert einen Hugging-Face-Token mit fein abgestimmter Inference-Berechtigung.",
        true, "", "HF-Token"),
    ProviderPreset("custom-chat", "Benutzerdefinierter OpenAI-kompatibler Chat", "openai-chat", "https://api.example.com/v1", "",
        "Modell-ID, die Ihr Server akzeptiert.",
        "HTTPS-API-Stammadresse für Chat Completions.",
        "Verbindung zu einem benutzerdefinierten OpenAI-kompatiblen Chat-Completions-Server.",
        true, "", "API-Schlüssel"),
    ProviderPreset("custom-responses", "Benutzerdefinierte OpenAI-kompatible Responses-API", "openai-responses", "https://api.example.com/v1", "",
        "Modell-ID, die Ihr Server akzeptiert.",
        "HTTPS-API-Stammadresse für Responses.",
        "Verbindung zu einem benutzerdefinierten OpenAI-kompatiblen Responses-Server.",
        true, "", "API-Schlüssel"),
    ProviderPreset("custom-anthropic", "Benutzerdefinierter Anthropic-kompatibler Endpunkt", "anthropic-messages", "https://api.example.com/v1", "",
        "Modell-ID, die Ihr Server akzeptiert.",
        "HTTPS-API-Stammadresse für das Anthropic-Messages-Protokoll.",
        "Verbindung zu einem benutzerdefinierten Anthropic-kompatiblen Messages-Server.",
        true, "", "API-Schlüssel"),
    ProviderPreset("azure-openai-chat", "Azure OpenAI Chat", "azure-openai-chat", "https://YOUR_RESOURCE.openai.azure.com/openai/v1", "",
        "Azure-Bereitstellungsname des gehosteten Modells, nicht die Publisher-Modell-ID.",
        "Azure-Ressourcen-URL mit /openai/v1 am Ende.",
        "Azure-gehostete OpenAI-Modelle verwenden den Bereitstellungsnamen. Ersetzen Sie YOUR_RESOURCE durch den Ressourcennamen.",
        true, "", "Azure-Ressourcen-API-Schlüssel"),
    ProviderPreset("azure-openai-responses", "Azure OpenAI Responses", "azure-openai-responses", "https://YOUR_RESOURCE.openai.azure.com/openai/v1", "",
        "Azure-Bereitstellungsname des gehosteten Modells, nicht die Publisher-Modell-ID.",
        "Azure-Ressourcen-URL mit /openai/v1 am Ende.",
        "Azure-gehostete OpenAI-Modelle verwenden den Bereitstellungsnamen. Ersetzen Sie YOUR_RESOURCE durch den Ressourcennamen.",
        true, "", "Azure-Ressourcen-API-Schlüssel"),
    ProviderPreset("azure-foundry-chat", "Azure AI Foundry Chat", "azure-foundry-chat", "https://YOUR_RESOURCE.services.ai.azure.com/openai/v1", "",
        "In Azure AI Foundry konfigurierter Bereitstellungsname.",
        "Azure-AI-Foundry-Ressourcen-URL mit /openai/v1 am Ende.",
        "In Azure AI Foundry bereitgestellte Modelle über die OpenAI-kompatible Chat-Completions-API.",
        true, "", "Azure-Ressourcen-API-Schlüssel"),
    ProviderPreset("azure-foundry-anthropic", "Azure AI Foundry Anthropic", "azure-foundry-anthropic", "https://YOUR_RESOURCE.services.ai.azure.com/anthropic", "",
        "Anthropic-Bereitstellungsname in Azure AI Foundry, nicht die Publisher-Modell-ID.",
        "Azure-AI-Foundry-Ressourcen-URL mit /anthropic oder /anthropic/v1 am Ende.",
        "Anthropic-Publisher-Modelle in Azure AI Foundry über die Anthropic-kompatible API.",
        true, "", "Azure-Ressourcen-API-Schlüssel"),
    ProviderPreset("bedrock-chat", "Amazon Bedrock Chat", "bedrock-chat", "https://bedrock-runtime.us-east-1.amazonaws.com/openai/v1", "",
        "Für Bedrocks OpenAI-kompatible API freigegebene Modell-ID des gehosteten Publisher-Modells.",
        "Bedrock-Runtime-Route /openai/v1 oder Bedrock-Mantle-Host mit .api.aws und /v1-Pfad.",
        "Verwendet einen Bedrock-API-Schlüssel als Bearer-Token. Verfügbar sind nur Modelle für die OpenAI-kompatible API; SigV4-IAM und die Converse-API werden nicht verwendet.",
        true, "", "Amazon-Bedrock-API-Schlüssel"),
    ProviderPreset("bedrock-responses", "Amazon Bedrock Responses", "bedrock-responses", "https://bedrock-runtime.us-east-1.amazonaws.com/openai/v1", "",
        "Für Bedrocks OpenAI-kompatible API freigegebene Modell-ID des gehosteten Publisher-Modells.",
        "Bedrock-Runtime-Route /openai/v1 oder Bedrock-Mantle-Host mit .api.aws und /v1-Pfad.",
        "Verwendet einen Bedrock-API-Schlüssel als Bearer-Token. Verfügbar sind nur Modelle für die OpenAI-kompatible API; SigV4-IAM und die Converse-API werden nicht verwendet.",
        true, "", "Amazon-Bedrock-API-Schlüssel"),
    ProviderPreset("vertex-openai", "Google Vertex AI OpenAI-compatible", "vertex-openai", "https://aiplatform.googleapis.com/v1/projects/YOUR_PROJECT/locations/global/endpoints/openapi", "",
        "Publisher-Modell-ID, die der OpenAI-kompatible Vertex-Endpunkt akzeptiert.",
        "Vertex-Route: /v1/projects/{project}/locations/{location}/endpoints/openapi.",
        "Publisher-Modelle auf Vertex AI. Geben Sie ein KURZLEBIGES Google-OAuth-Zugriffstoken manuell ein und erneuern Sie es nach Ablauf. Google-Anmeldung und Token-Erneuerung werden nicht angeboten.",
        true, "", "Google-OAuth-Zugriffstoken"),
    ProviderPreset("vertex-claude", "Google Vertex AI Claude (native)", "anthropic-messages", "https://aiplatform.googleapis.com", "",
        "Anthropic-Publisher-Modell-ID.",
        "Die native Vertex-rawPredict-Route wird vom Anthropic-Transport dieser App nicht unterstützt.",
        "Vertex-gehostetes Claude benötigt den nativen rawPredict-Transport, der von dieser App nicht unterstützt wird.",
        false, "Der native Vertex-Claude-rawPredict-Transport wird nicht unterstützt.", "API-Schlüssel"),
    ProviderPreset("bedrock-native", "Amazon Bedrock Claude / Converse", "anthropic-messages", "https://bedrock-runtime.us-east-1.amazonaws.com", "",
        "Bedrock-Modell-ID.",
        "Die nativen Bedrock-Claude- und Converse-Routen werden vom Protokoll dieser App nicht unterstützt.",
        "Bedrock Claude und Converse benötigen Transporte, die diese App nicht unterstützt.",
        false, "Die nativen Bedrock-Claude- und Converse-Transporte werden nicht unterstützt.", "API-Schlüssel"),
)

internal fun presetById(id: String): ProviderPreset? = PROVIDER_PRESETS.firstOrNull { it.id == id }

/** Suche ueber Anzeigename, Id und Beschreibung (ohne Gross-/Kleinschreibung). */
internal fun searchPresets(query: String, group: String): List<ProviderPreset> {
    val q = query.trim().lowercase(Locale.ROOT)
    return PROVIDER_PRESETS.filter { p ->
        (group == "Alle" || p.group == group) &&
            (q.isEmpty() || p.label.lowercase(Locale.ROOT).contains(q) || p.id.contains(q) ||
                p.description.lowercase(Locale.ROOT).contains(q))
    }
}

/** Ordnet eine gespeicherte Route (api-Familie + Endpunkt) wieder einer Vorlage zu. */
internal fun inferPreset(api: String, endpoint: String): ProviderPreset? {
    val uri = parseHttps(endpoint) ?: return PROVIDER_PRESETS.firstOrNull { it.api == api && it.id.startsWith("custom-") }
    val host = uri.host.lowercase(Locale.ROOT)
    val path = uri.path.orEmpty().trimEnd('/')
    PROVIDER_PRESETS.firstOrNull { p ->
        !p.id.startsWith("custom-") && p.available && p.api == api &&
            parseHttps(p.endpoint)?.let { it.host.equals(host, true) && it.path.orEmpty().trimEnd('/') == path } == true
    }?.let { return it }
    return PROVIDER_PRESETS.firstOrNull { it.api == api && it.id.startsWith("custom-") }
}

private fun parseHttps(text: String): URI? = try {
    val uri = URI(text.trim())
    if (uri.scheme.equals("https", true) && !uri.host.isNullOrEmpty()) uri else null
} catch (_: Exception) {
    null
}

private val BEDROCK_RUNTIME = Regex("^bedrock-runtime\\.[^.]+\\.amazonaws\\.com$")
private val BEDROCK_MANTLE = Regex("^bedrock-mantle\\.[^.]+\\.api\\.aws$")
private val VERTEX_HOST = Regex("^[^.]+-aiplatform\\.googleapis\\.com$")
private val VERTEX_PATH = Regex("^/v1/projects/[^/]+/locations/[^/]+/endpoints/openapi$")

private fun routeAllowed(preset: ProviderPreset, host: String, path: String, port: Int): Boolean = when {
    preset.id == "huggingface" -> host == "router.huggingface.co" && path == "/v1" && (port == -1 || port == 443)
    preset.id.startsWith("azure-openai") -> host.endsWith(".openai.azure.com") && path == "/openai/v1"
    preset.id == "azure-foundry-chat" -> host.endsWith(".services.ai.azure.com") && path == "/openai/v1"
    preset.id == "azure-foundry-anthropic" ->
        host.endsWith(".services.ai.azure.com") && (path == "/anthropic" || path == "/anthropic/v1")
    preset.id.startsWith("bedrock-") ->
        (BEDROCK_RUNTIME.matches(host) && path == "/openai/v1") || (BEDROCK_MANTLE.matches(host) && path.startsWith("/v1"))
    preset.id == "vertex-openai" ->
        (host == "aiplatform.googleapis.com" || VERTEX_HOST.matches(host)) && VERTEX_PATH.matches(path)
    else -> true
}

/** Deutsche Fehlermeldung oder `null`, wenn Endpunkt und Modell zur Vorlage passen. */
internal fun validatePreset(preset: ProviderPreset, endpoint: String, model: String): String? {
    if (!preset.available) return preset.unavailableReason
    val uri = parseHttps(endpoint) ?: return "Geben Sie eine gültige HTTPS-Endpunkt-URL ein."
    if (!uri.userInfo.isNullOrEmpty() || uri.rawQuery != null || uri.rawFragment != null) {
        return "Die Endpunkt-URL darf weder Zugangsdaten noch Query oder Fragment enthalten."
    }
    if (endpoint.uppercase(Locale.ROOT).contains("YOUR_")) {
        return "Ersetzen Sie den Platzhalter YOUR_… in der Endpunkt-URL durch Ihren Ressourcen- oder Projektnamen."
    }
    val path = uri.path.orEmpty().trimEnd('/')
    if (!routeAllowed(preset, uri.host.lowercase(Locale.ROOT), path, uri.port)) {
        return "Die Endpunkt-URL passt nicht zu ${preset.label}. ${preset.endpointHint}"
    }
    val m = model.trim()
    if (m.isEmpty()) return "Geben Sie eine Modell-ID oder einen Bereitstellungsnamen ein."
    if (m.length > 200 || m.any { it.isISOControl() }) return "Die Modell-ID ist zu lang oder enthält Steuerzeichen."
    return null
}

/** Kurze deutsche Hinweise zu bekannten Anbieterfehlern (ProviderErrorText des Kits). */
internal fun friendlyProviderError(message: String): String {
    val key = message.lowercase(Locale.ROOT)
    return when {
        "invalid x-api-key" in key ->
            "Der Anbieter hat den API-Schlüssel abgelehnt. Prüfe den Schlüssel für diesen Endpunkt."
        "subscription_sharing_usage_limit_exceeded" in key ->
            "Das ChatGPT-Kontingent ist ausgeschöpft. Prüfe das Nutzungslimit in deinem Konto."
        "subscription_sharing_usage_unavailable" in key ->
            "Die Abo-Nutzung ist für dieses ChatGPT-Konto derzeit nicht verfügbar."
        "codex response stream reported a failure" in key ->
            "Der ChatGPT-Stream ist fehlgeschlagen. Der Fehlercode fehlt. Prüfe Konto und Modellauswahl."
        else -> message
    }
}
