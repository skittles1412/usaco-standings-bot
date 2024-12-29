# USACO Standings Scraper

This crate scrapes USACO webpages to extract historical USACO result data. Run `cargo doc --open` to see the documentation of this crate.

The actual pages we scrape are:
- Contest result pages such as [this one](https://usaco.org/current/data/open24_platinum_results.html).
- USACO finalist announcement pages such as [this one](https://usaco.org/index.php?page=finalists24).
- The USACO [history page](https://usaco.org/index.php?page=history), which contains IOI and EGOI results.

## Historical changes and quirks
We scrape all data on those pages starting from the 2011-12 season, which is when USACO migrated to its new website. Some notable changes since then:
- In the 2013-14 season and earlier, complete results, including those who didn't promote, were released.
- The 2014-15 season was the first season that switched to the current 4 contests per year format. Previously there were 6 contests per year (November, December, January, February, March, and US Open).
- The platinum division was introduced in the 2015-16 season. Previously, there was only bronze, silver, and gold.
- Starting from the 2020-21 season, USACO no longer releases lists of promotions for bronze and silver.
- The US has been participating in EGOI since 2021 (the first ever EGOI).
- Starting from the 2021-22 season, some students were invited to the US training camp specifically for EGOI selection.

There have so far been two weird contests:
- [November 2011 Bronze](https://usaco.org/current/data/nov11_bronze_results.html) had four problems.
- [Open 2017 Gold](https://usaco.org/current/data/open17_gold_results.html) had an incorrect problem. Scores were calculated with that incorrect problem thrown out, but students who met the qualifying threshold when their score was calculated with the broken problem still promoted.

## Robustness
The scrapers are designed to be robust. When faced with unexpected / malformed input, they will do their best to parse what they can and log relevant warnings using `tracing`. The parsing functions should never panic. The parsers should all work correctly as of December 2024.

## Examples

See `examples/scrape.rs` for an example on how to use the scraper.
