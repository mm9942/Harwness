package dev.harw.mobile

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import androidx.compose.foundation.background
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.LinkAnnotation
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.TextLinkStyles
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.text.withLink
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/** Inline span of a markdown line. */
sealed interface MdInline {
    data class Plain(val text: String) : MdInline
    data class Bold(val text: String) : MdInline
    data class Italic(val text: String) : MdInline
    data class Code(val text: String) : MdInline
    data class Link(val label: String, val url: String) : MdInline
}

/** Block of a markdown document. */
sealed interface MdBlock {
    data class Paragraph(val text: String) : MdBlock
    data class Heading(val level: Int, val text: String) : MdBlock
    data class CodeBlock(val language: String, val code: String) : MdBlock
    data class ListItem(val depth: Int, val marker: String, val text: String) : MdBlock
    data class Quote(val text: String) : MdBlock
    data object Rule : MdBlock
}

private val headingRe = Regex("^(#{1,6})\\s+(.*?)\\s*#*\\s*$")
private val bulletRe = Regex("^(\\s*)[-*+]\\s+(.*)$")
private val orderedRe = Regex("^(\\s*)(\\d{1,9})[.)]\\s+(.*)$")
private val ruleRe = Regex("^\\s{0,3}([-*_])(\\s*\\1){2,}\\s*$")

/** Splits markdown into blocks. Unclosed code fences run to the end of the text. */
fun parseMarkdown(source: String): List<MdBlock> {
    val out = ArrayList<MdBlock>()
    val lines = source.replace("\r\n", "\n").replace('\r', '\n').split('\n')
    val para = StringBuilder()
    fun flush() {
        if (para.isNotEmpty()) {
            out += MdBlock.Paragraph(para.toString())
            para.setLength(0)
        }
    }
    var i = 0
    while (i < lines.size) {
        val line = lines[i]
        val trimmed = line.trimStart()
        if (trimmed.startsWith("```") || trimmed.startsWith("~~~")) {
            flush()
            val fence = trimmed.take(3)
            val lang = trimmed.drop(3).trim().substringBefore(' ')
            val body = ArrayList<String>()
            i++
            while (i < lines.size && !lines[i].trimStart().startsWith(fence)) {
                body += lines[i]
                i++
            }
            out += MdBlock.CodeBlock(lang, body.joinToString("\n"))
            i++
            continue
        }
        val heading = headingRe.matchEntire(line)
        val ordered = orderedRe.matchEntire(line)
        val bullet = bulletRe.matchEntire(line)
        when {
            line.isBlank() -> flush()
            ruleRe.matches(line) -> { flush(); out += MdBlock.Rule }
            heading != null -> {
                flush()
                out += MdBlock.Heading(heading.groupValues[1].length, heading.groupValues[2])
            }
            ordered != null -> {
                flush()
                out += MdBlock.ListItem(
                    ordered.groupValues[1].length / 2,
                    ordered.groupValues[2] + ".",
                    ordered.groupValues[3],
                )
            }
            bullet != null -> {
                flush()
                out += MdBlock.ListItem(bullet.groupValues[1].length / 2, "•", bullet.groupValues[2])
            }
            trimmed.startsWith(">") -> {
                flush()
                out += MdBlock.Quote(trimmed.removePrefix(">").trim())
            }
            else -> {
                if (para.isNotEmpty()) para.append('\n')
                para.append(line.trim())
            }
        }
        i++
    }
    flush()
    return out
}

/** Only these schemes are made clickable. */
fun isSafeLink(url: String): Boolean {
    val u = url.trim().lowercase()
    return u.startsWith("https://") || u.startsWith("http://") || u.startsWith("mailto:")
}

/** Parses `code`, **bold**, *italic*, _italic_ and [label](url). Unclosed markers stay literal. */
fun parseInline(text: String): List<MdInline> {
    val out = ArrayList<MdInline>()
    val plain = StringBuilder()
    fun flush() {
        if (plain.isNotEmpty()) {
            out += MdInline.Plain(plain.toString())
            plain.setLength(0)
        }
    }
    var i = 0
    while (i < text.length) {
        val c = text[i]
        if (c == '\\' && i + 1 < text.length && text[i + 1] in "\\`*_[]()#>-+.!") {
            plain.append(text[i + 1])
            i += 2
            continue
        }
        if (c == '`') {
            val end = text.indexOf('`', i + 1)
            if (end > i + 1) {
                flush()
                out += MdInline.Code(text.substring(i + 1, end))
                i = end + 1
                continue
            }
        }
        if (c == '*' && text.startsWith("**", i)) {
            val end = text.indexOf("**", i + 2)
            if (end > i + 2) {
                flush()
                out += MdInline.Bold(text.substring(i + 2, end))
                i = end + 2
                continue
            }
        }
        if (c == '*' || c == '_') {
            val end = text.indexOf(c, i + 1)
            val intraWord = c == '_' && i > 0 && text[i - 1].isLetterOrDigit()
            if (end > i + 1 && !intraWord && !text[i + 1].isWhitespace() && !text[end - 1].isWhitespace()) {
                flush()
                out += MdInline.Italic(text.substring(i + 1, end))
                i = end + 1
                continue
            }
        }
        if (c == '[') {
            val close = text.indexOf("](", i + 1)
            val end = if (close > 0) text.indexOf(')', close + 2) else -1
            if (close > i && end > close) {
                val label = text.substring(i + 1, close)
                val url = text.substring(close + 2, end).trim()
                flush()
                out += if (isSafeLink(url)) MdInline.Link(label, url) else MdInline.Plain(label)
                i = end + 1
                continue
            }
        }
        plain.append(c)
        i++
    }
    flush()
    return out
}

/** Plain text of a document without markup, for tests and previews. */
fun markdownToPlain(source: String): String =
    parseMarkdown(source).joinToString("\n") { block ->
        when (block) {
            is MdBlock.Paragraph -> inlinePlain(block.text)
            is MdBlock.Heading -> inlinePlain(block.text)
            is MdBlock.CodeBlock -> block.code
            is MdBlock.ListItem -> block.marker + " " + inlinePlain(block.text)
            is MdBlock.Quote -> inlinePlain(block.text)
            MdBlock.Rule -> ""
        }
    }

private fun inlinePlain(text: String): String =
    parseInline(text).joinToString("") {
        when (it) {
            is MdInline.Plain -> it.text
            is MdInline.Bold -> it.text
            is MdInline.Italic -> it.text
            is MdInline.Code -> it.text
            is MdInline.Link -> it.label
        }
    }

private fun copyToClipboard(context: Context, label: String, text: String) {
    val cm = context.getSystemService(Context.CLIPBOARD_SERVICE) as? ClipboardManager ?: return
    cm.setPrimaryClip(ClipData.newPlainText(label, text))
}

@Composable
private fun inlineAnnotated(text: String): AnnotatedString {
    val codeBg = MaterialTheme.colorScheme.surfaceContainerHigh
    val linkColor = MaterialTheme.colorScheme.primary
    return buildAnnotatedString {
        for (span in parseInline(text)) {
            when (span) {
                is MdInline.Plain -> append(span.text)
                is MdInline.Bold -> withStyle(SpanStyle(fontWeight = FontWeight.Bold)) { append(span.text) }
                is MdInline.Italic -> withStyle(SpanStyle(fontStyle = FontStyle.Italic)) { append(span.text) }
                is MdInline.Code -> withStyle(SpanStyle(fontFamily = FontFamily.Monospace, background = codeBg)) {
                    append(span.text)
                }
                is MdInline.Link -> withLink(
                    LinkAnnotation.Url(
                        span.url,
                        TextLinkStyles(SpanStyle(color = linkColor, textDecoration = TextDecoration.Underline)),
                    ),
                ) { append(span.label) }
            }
        }
    }
}

/** Renders markdown: headings, lists, quotes, links, inline code and code blocks with a copy button. */
@Composable
fun MarkdownText(text: String, modifier: Modifier = Modifier) {
    val blocks = remember(text) { parseMarkdown(text) }
    SelectionContainer {
        Column(modifier, verticalArrangement = Arrangement.spacedBy(6.dp)) {
            for (block in blocks) {
                when (block) {
                    is MdBlock.Paragraph -> Text(inlineAnnotated(block.text), style = MaterialTheme.typography.bodyMedium)
                    is MdBlock.Heading -> Text(
                        inlineAnnotated(block.text),
                        style = when (block.level) {
                            1 -> MaterialTheme.typography.titleLarge
                            2 -> MaterialTheme.typography.titleMedium
                            else -> MaterialTheme.typography.titleSmall
                        },
                        fontWeight = FontWeight.SemiBold,
                    )
                    is MdBlock.ListItem -> Row(Modifier.padding(start = (12 * block.depth).dp)) {
                        Text(block.marker, style = MaterialTheme.typography.bodyMedium, modifier = Modifier.padding(end = 8.dp))
                        Text(inlineAnnotated(block.text), style = MaterialTheme.typography.bodyMedium)
                    }
                    is MdBlock.Quote -> Row(Modifier.fillMaxWidth()) {
                        Box(
                            Modifier
                                .padding(end = 8.dp)
                                .background(MaterialTheme.colorScheme.outline, RoundedCornerShape(2.dp))
                                .padding(horizontal = 1.dp),
                        ) { Text(" ", style = MaterialTheme.typography.bodyMedium) }
                        Text(
                            inlineAnnotated(block.text),
                            style = MaterialTheme.typography.bodyMedium,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                    MdBlock.Rule -> HorizontalDivider()
                    is MdBlock.CodeBlock -> CodeBlockView(block)
                }
            }
        }
    }
}

@Composable
private fun CodeBlockView(block: MdBlock.CodeBlock) {
    val context = LocalContext.current
    var copied by remember { mutableStateOf(false) }
    Column(
        Modifier
            .fillMaxWidth()
            .background(MaterialTheme.colorScheme.surfaceContainerHigh, RoundedCornerShape(8.dp))
            .padding(8.dp),
    ) {
        Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
            Text(
                block.language,
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(top = 10.dp),
            )
            TextButton(onClick = {
                copyToClipboard(context, "code", block.code)
                copied = true
            }) { Text(if (copied) "Kopiert" else "Kopieren") }
        }
        Text(
            block.code,
            fontFamily = FontFamily.Monospace,
            fontSize = 13.sp,
            softWrap = false,
            modifier = Modifier.horizontalScroll(rememberScrollState()),
        )
    }
}
