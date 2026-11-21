package dev.harw.mobile

import java.io.File
import java.nio.file.Files
import java.nio.file.LinkOption
import java.nio.file.NoSuchFileException
import java.nio.file.attribute.BasicFileAttributes

/** Detect both complete and interrupted writes without treating IO errors as absence. */
internal fun atomicFileExists(base: File): Boolean =
    listOf(base, File(base.path + ".new"), File(base.path + ".bak")).any { file ->
        try {
            Files.readAttributes(file.toPath(), BasicFileAttributes::class.java, LinkOption.NOFOLLOW_LINKS)
            true
        } catch (_: NoSuchFileException) {
            false
        }
    }
