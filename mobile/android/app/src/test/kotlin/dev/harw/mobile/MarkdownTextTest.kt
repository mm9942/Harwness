package dev.harw.mobile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class MarkdownTextTest {
    @Test
    fun parsesHeadingsListsAndParagraphs() {
        val blocks = parseMarkdown("# Titel\n\nText eins\nText zwei\n\n- a\n  - b\n1. x\n---")
        assertEquals(MdBlock.Heading(1, "Titel"), blocks[0])
        assertEquals(MdBlock.Paragraph("Text eins\nText zwei"), blocks[1])
        assertEquals(MdBlock.ListItem(0, "•", "a"), blocks[2])
        assertEquals(MdBlock.ListItem(1, "•", "b"), blocks[3])
        assertEquals(MdBlock.ListItem(0, "1.", "x"), blocks[4])
        assertEquals(MdBlock.Rule, blocks[5])
    }

    @Test
    fun codeFenceKeepsContentVerbatim() {
        val blocks = parseMarkdown("vor\n```rust\nlet a = *b*;\n# kein Titel\n```\nnach")
        assertEquals(3, blocks.size)
        assertEquals(MdBlock.CodeBlock("rust", "let a = *b*;\n# kein Titel"), blocks[1])
    }

    @Test
    fun unclosedFenceRunsToEnd() {
        val blocks = parseMarkdown("```\nzeile")
        assertEquals(listOf<MdBlock>(MdBlock.CodeBlock("", "zeile")), blocks)
    }

    @Test
    fun parsesInlineStyles() {
        val spans = parseInline("a **fett** und *kursiv* und `code` und [Doku](https://example.org/x)")
        assertTrue(MdInline.Bold("fett") in spans)
        assertTrue(MdInline.Italic("kursiv") in spans)
        assertTrue(MdInline.Code("code") in spans)
        assertTrue(MdInline.Link("Doku", "https://example.org/x") in spans)
    }

    @Test
    fun unsafeLinksBecomePlainLabels() {
        val spans = parseInline("[klick](javascript:alert(1))")
        assertFalse(spans.any { it is MdInline.Link })
        assertFalse(isSafeLink("javascript:alert(1)"))
        assertTrue(isSafeLink("mailto:a@example.org"))
    }

    @Test
    fun unclosedMarkersStayLiteral() {
        assertEquals(listOf<MdInline>(MdInline.Plain("**offen und `auch")), parseInline("**offen und `auch"))
        assertEquals(listOf<MdInline>(MdInline.Plain("snake_case_name")), parseInline("snake_case_name"))
    }

    @Test
    fun escapesAndPlainText() {
        assertEquals("*kein* Stern", markdownToPlain("\\*kein\\* Stern"))
        assertEquals("Titel\n• punkt", markdownToPlain("## Titel\n- **punkt**"))
    }
}
