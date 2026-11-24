package dev.harw.mobile

import java.text.Collator
import java.util.Locale

internal enum class HomeGrouping { Projects, Sessions }

internal enum class HomeListSort(val labelRes: Int) {
    Recent(R.string.unified_home_recent_sort),
    Oldest(R.string.unified_home_oldest_sort),
    Alphabetical(R.string.unified_home_az),
    ReverseAlphabetical(R.string.unified_home_za),
}


internal fun <T> orderUnifiedHome(
    rows: List<T>,
    sort: HomeListSort,
    title: (T) -> String,
    date: (T) -> Long?,
    id: (T) -> String,
    locale: Locale = Locale.getDefault(),
): List<T> {
    val alphabet = Collator.getInstance(locale).apply { strength = Collator.SECONDARY }
    val byLabel = Comparator<T> { first, second -> alphabet.compare(title(first), title(second)) }
    val tieBreak = compareBy<T> { id(it) }
    val order = when (sort) {
        HomeListSort.Recent -> compareBy<T> { date(it) == null }.thenByDescending(date)
        HomeListSort.Oldest -> compareBy<T> { date(it) == null }.thenBy(date)
        HomeListSort.Alphabetical -> byLabel
        HomeListSort.ReverseAlphabetical -> byLabel.reversed()
    }
    return rows.sortedWith(order.then(tieBreak))
}
