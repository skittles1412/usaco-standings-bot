//! Scrapes historical USACO scoreboards.
//!
//! The scrapers in this library are designed to be robust, and when faced with
//! unexpected data, they will do the best to parse what they can and will never
//! panic or error.

use anyhow::anyhow;
use http::StatusCode;
use scraper::{ElementRef, Html, Node, Selector};
use std::{collections::HashSet, future::Future};
use tokio::task::JoinSet;
use tracing::{debug, instrument, warn};
use url::Url;

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

/// Month of a USACO competition, or "open" to refer to the US Open. Contains 6
/// months since USACO used to be held 6 times a year.
#[derive(Debug, Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum Month {
    January,
    February,
    /// Refers to the March contests back when USACO had 6 contests a year.
    /// Different from what is now the US Open.
    March,
    Open,
    November,
    December,
}

impl Month {
    /// The short lowercase version of the month name used in the USACO result
    /// URLs.
    fn url_name(self) -> &'static str {
        match self {
            Month::January => "jan",
            Month::February => "feb",
            Month::March => "mar",
            Month::Open => "open",
            Month::November => "nov",
            Month::December => "dec",
        }
    }
}

/// A month, year tuple specifying the time a contest was held.
#[derive(Debug, Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct MonthYear {
    pub year: u16,
    pub month: Month,
}

/// The division of a contest. Order goes bronze < silver < gold < plat.
#[derive(Debug, Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum Division {
    Bronze,
    Silver,
    Gold,
    Platinum,
}

impl Division {
    /// The lowercase version of the division name used in the USACO result
    /// URLs.
    fn url_name(self) -> &'static str {
        match self {
            Division::Bronze => "bronze",
            Division::Silver => "silver",
            Division::Gold => "gold",
            Division::Platinum => "platinum",
        }
    }
}

/// The graduation date of a student.
#[derive(Debug, Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum Graduation {
    HighSchool { year: u16 },
    Observer,
}

/// The result of a specific testcase for a problem.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum TestcaseResult {
    Correct,
    WrongAnswer,
    Timeout,
    CompilationError,
    RunTimeError,
    Empty,
}

/// A contest participant that showed up on the leaderboard.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct ContestParticipant {
    pub country: String,
    pub graduation: Graduation,
    pub name: String,
    pub score: u16,
    /// The results of their last submission for each of the problems. `None` if
    /// the contestant didn't submit to the problem.
    ///
    /// Typically, this should be 3 problems, with two exceptions (so far):
    /// - 2011 November Bronze had 4 problems
    /// - 2017 Open Gold had a problem thrown out, and for some contestants,
    ///   only their scores but not submission results were revealed
    pub submission_results: Vec<Option<Vec<TestcaseResult>>>,
}

/// All the data on a contest page.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Contest {
    pub time: MonthYear,
    pub division: Division,
    pub participants: Vec<ContestParticipant>,
}

/// A participant in a USACO camp.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct CampParticipant {
    pub graduation_year: u16,
    pub name: String,
    pub school: String,
    pub state: String,
    /// Whether the participant was invited as an EGOI finalist.
    pub is_egoi: bool,
}

/// All the data on a USACO finalists page.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Camp {
    /// The year the camp was held. For example, this would be 2024 for the
    /// 2023-24 season USACO camp.
    pub year: u16,
    pub participants: Vec<CampParticipant>,
}

/// Medal of a participant at IOI or EGOI.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum IntlMedal {
    /// Couldn't attend due to visa issues (2017).
    VisaIssue,
    NoMedal,
    Bronze,
    Silver,
    Gold,
}

/// A US team member at a specific year of IOI or EGOI.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct IntlParticipant {
    /// Year of the IOI or EGOI.
    pub year: u16,
    pub result: IntlMedal,
    pub name: String,
}

/// All the data on the [history](https://usaco.org/index.php?page=history) page (IOI and EGOI results).
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct IntlHistory {
    pub ioi: Vec<IntlParticipant>,
    pub egoi: Vec<IntlParticipant>,
}

/// The heart of this crate. Contains data we scrape from the USACO website.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct UsacoData {
    pub contests: Vec<Contest>,
    pub camps: Vec<Camp>,
    pub intl_history: IntlHistory,
}

/// Normalize text nodes by dealing with nbsps and duplicate whitespace.
fn normalize_text(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The text content of `e`, normalized using [`normalize_text`].
fn elem_text(e: ElementRef) -> String {
    normalize_text(&e.text().collect::<String>())
}

/// Parses a contest results page, such as [this one](https://usaco.org/current/data/open24_platinum_results.html).
/// This function should never panic. Instead, it will ignore unexpected data.
#[instrument(skip(html))]
pub fn parse_contest_page(time: MonthYear, division: Division, html: &str) -> Contest {
    let doc = Html::parse_document(html);

    let table_selector = Selector::parse("table").unwrap();
    let tr_selector = Selector::parse("tr").unwrap();
    let th_selector = Selector::parse("th").unwrap();
    let td_selector = Selector::parse("td").unwrap();

    let mut participants = vec![];

    for table in doc.select(&table_selector) {
        let mut rows = table.select(&tr_selector);

        let (observers, col_widths) = match || -> anyhow::Result<_> {
            // first row is header row (USACO doesn't use <thead>, instead all rows get
            // stuffed into <tbody>)
            let headers = rows.next().ok_or_else(|| anyhow!("missing header row"))?;
            let headers_text = headers
                .select(&th_selector)
                .map(elem_text)
                .collect::<Vec<_>>();

            // observers have their graduation year omitted.
            let observers = headers_text[1] != "Year";

            // columns look like:
            // country, year?, name, score, blank, p1, blank, p2, blank, p3
            // where each testcase result of a problem is its own column, so col_widths
            // roughly stores the number of testcases for each problem. it seems like
            // there's a blank <td> at the end of each problem and part of its colspan
            // though.
            let Some(col_widths) = headers
                .select(&th_selector)
                .skip(if observers { 3 } else { 4 })
                .enumerate()
                .filter_map(|(i, x)| (i % 2 == 1).then_some(x))
                .map(|c| c.attr("colspan").and_then(|c| c.parse::<u8>().ok()))
                .collect::<Option<Vec<_>>>()
            else {
                anyhow::bail!("failed to parse colspan of problems");
            };

            Ok((observers, col_widths))
        }() {
            Ok(x) => x,
            Err(e) => {
                warn!("error when parsing table: {e:?}");
                continue;
            }
        };

        // parse each row of the standings
        for row in rows {
            let res = || -> anyhow::Result<_> {
                let mut cells = row.select(&td_selector).map(elem_text);
                let mut next_cell = || cells.next().ok_or_else(|| anyhow!("row is missing cells"));

                let country = next_cell()?;
                let graduation = if observers {
                    Graduation::Observer
                } else {
                    Graduation::HighSchool {
                        year: next_cell()?.parse()?,
                    }
                };
                let name = next_cell()?;
                let score = next_cell()?.parse()?;

                let mut submission_results = vec![];
                for &col_width in &col_widths {
                    // this column should be an empty <td>
                    next_cell()?;

                    // the actual testcase results
                    let mut problem_res = (0..col_width)
                        .map(|_| next_cell())
                        .collect::<Result<Vec<_>, _>>()?;

                    // seems like there's just a trailing empty td after each problem for some
                    // reason
                    if matches!(problem_res.last(), Some(s) if s.is_empty()) {
                        problem_res.pop();
                    }

                    if problem_res.iter().all(|s| s.is_empty()) {
                        // no submission
                        submission_results.push(None);
                        continue;
                    }

                    submission_results.push(Some(
                        problem_res
                            .into_iter()
                            .map(|s| match &*s {
                                "*" => Ok(TestcaseResult::Correct),
                                "x" => Ok(TestcaseResult::WrongAnswer),
                                "t" => Ok(TestcaseResult::Timeout),
                                "c" => Ok(TestcaseResult::CompilationError),
                                // was `s` on old USACO result pages
                                "s" | "!" => Ok(TestcaseResult::RunTimeError),
                                "e" => Ok(TestcaseResult::Empty),
                                _ => Err(anyhow!("unrecognized testcase result '{s}'")),
                            })
                            .collect::<Result<Vec<_>, _>>()?,
                    ));
                }

                participants.push(ContestParticipant {
                    country,
                    graduation,
                    name,
                    score,
                    submission_results,
                });

                Ok(())
            }();

            if let Err(e) = res {
                warn!("error when parsing row `{}`: {e:?}", row.html());
            }
        }
    }

    // deal with duplicate entries in pre-college global vs pre-college US
    {
        let mut vis = HashSet::new();
        participants.retain(|c| vis.insert(c.clone()));
    }

    Contest {
        time,
        division,
        participants,
    }
}

/// Parses a USACO finalists announcement page, such as [this one](https://usaco.org/index.php?page=finalists24).
/// This function should never panic. Instead, it will ignore unexpected data.
#[instrument(skip(html))]
pub fn parse_camp_page(camp_year: u16, html: &str) -> Camp {
    let doc = Html::parse_document(html);

    let table_selector = Selector::parse("table").unwrap();
    let tr_selector = Selector::parse("tr").unwrap();
    let td_selector = Selector::parse("td").unwrap();

    let mut participants = vec![];

    for (table_ind, table) in doc.select(&table_selector).enumerate() {
        // should have at most two tables. second table, if it exists, should be EGOI
        // finalists.
        if table_ind >= 2 {
            warn!("camp page should only have at most two tables");
            continue;
        }

        // skip header row
        let rows = table.select(&tr_selector).skip(1);

        for row in rows {
            // just randomly appears on 14 and 24 for some reason.
            if row.inner_html() == "<td></td>" {
                continue;
            }

            let mut res = || -> anyhow::Result<_> {
                let cells = row.select(&td_selector).map(elem_text).collect::<Vec<_>>();

                let [graduation_year, name, school, state] = cells
                    .try_into()
                    .map_err(|_| anyhow::anyhow!("unexpected number of cells in row"))?;

                participants.push(CampParticipant {
                    graduation_year: graduation_year.parse()?,
                    name,
                    school,
                    state,
                    is_egoi: table_ind > 0,
                });

                Ok(())
            };

            if let Err(e) = res() {
                warn!("error when parsing row `{}`: {e:?}", row.html());
            }
        }
    }

    Camp {
        year: camp_year,
        participants,
    }
}

/// Parses [the history page](https://usaco.org/index.php?page=history).
/// This function should never panic. Instead, it will ignore unexpected data.
#[instrument(skip(html))]
pub fn parse_history_page(html: &str) -> IntlHistory {
    let doc = Html::parse_document(html);

    let outer_div_selector = Selector::parse(".content > div").unwrap();
    let inner_div_selector = Selector::parse("div.panel.historypanel").unwrap();
    let h2_selector = Selector::parse("h2").unwrap();

    let mut ioi = vec![];
    let mut egoi = vec![];

    // history page is split into two outer divs, one for ioi and another for egoi
    for outer in doc.select(&outer_div_selector) {
        let Some(heading) = outer.select(&h2_selector).next() else {
            continue;
        };
        let heading = heading.text().collect::<String>();

        let is_ioi = heading.contains("IOI");
        let is_egoi = heading.contains("EGOI");

        if is_ioi && is_egoi {
            warn!(
                "section contains both IOI and EGOI in its heading `{}`",
                outer.html()
            );
            continue;
        }

        if !is_ioi && !is_egoi {
            continue;
        }

        let mut results = vec![];

        // within each ioi/egoi outer div are inner divs corresponding to each year
        for year_div in outer.select(&inner_div_selector) {
            let Ok(year) = elem_text(year_div)[0..4].parse() else {
                warn!("failed to parse year of `{}`", year_div.html());
                continue;
            };

            // immediately before each contestant's text node should be an <img>
            // representing their medal, so we iterate over contestants and attempt to parse
            // their medal from the <img> sibling.
            for contestant in year_div.children() {
                let Node::Text(name) = contestant.value() else {
                    continue;
                };
                let name = name.trim();
                // happens because of the <br>s, I think
                if name.chars().all(char::is_whitespace) {
                    continue;
                }

                // visa issue, 2017
                if name.starts_with("(*)") {
                    results.push(IntlParticipant {
                        year,
                        name: name[4..].trim().to_string(),
                        result: IntlMedal::VisaIssue,
                    });
                    continue;
                }

                let mut res = || -> anyhow::Result<_> {
                    let medal = contestant
                        .prev_sibling()
                        .ok_or_else(|| anyhow!("no preceding medal <img> found for contestant"))?;
                    let Node::Element(medal) = medal.value() else {
                        anyhow::bail!("preceding node is not an element");
                    };

                    let result = match medal
                        .attr("src")
                        .ok_or_else(|| anyhow!("no src found for medal <img>"))?
                    {
                        "current/images/medal_none.png" => IntlMedal::NoMedal,
                        "current/images/medal_bronze.png" => IntlMedal::Bronze,
                        "current/images/medal_silver.png" => IntlMedal::Silver,
                        "current/images/medal_gold.png" => IntlMedal::Gold,
                        m => anyhow::bail!("unexpected medal {m}"),
                    };

                    // deal with things like "Rain Jiang (5th place)".
                    let name = if name.contains("place)") {
                        name[..name
                            .rfind('(')
                            .ok_or_else(|| anyhow!("missing '(' when 'place) parsed'"))?]
                            .trim()
                            .to_string()
                    } else {
                        name.to_string()
                    };

                    results.push(IntlParticipant { year, name, result });

                    Ok(())
                };

                if let Err(e) = res() {
                    warn!(
                        "error when parsing year `{}` and contestant `{:?}`: {e:?}",
                        year_div.html(),
                        contestant
                    );
                }
            }
        }

        if is_ioi {
            if !ioi.is_empty() {
                warn!("ioi parsed twice");
            }
            ioi = results;
        } else {
            if !egoi.is_empty() {
                warn!("egoi parsed twice");
            }
            egoi = results;
        }
    }

    // intentionally stable sort to preserve order the competitors are listed
    ioi.sort_by_key(|c| c.year);
    egoi.sort_by_key(|c| c.year);

    IntlHistory { ioi, egoi }
}

/// An HTTP client which can handle simple GET requests. This trait exists so
/// users are free to implement behavior such as rate limiting, custom user
/// agents, or progress reporting.
pub trait HttpClient {
    type Error;
    type Future: Future<Output = Result<(StatusCode, String), Self::Error>> + Send + 'static;

    fn get(&mut self, url: Url) -> Self::Future;
}

/// Parses all standings related data on the USACO website. Results are sorted
/// in increasing order of time and division.
///
/// `max_year` is the maximum year to parse until. If it's year 2025, for
/// example, standings up until and including the 2024-25 season will be parsed.
///
/// The provided `svc` should be a [`Service`] which takes in an HTTP URL and
/// responds with the result of GETting that URL. Here, we use tower services so
/// it is easy to make use of the tower ecosystem and add other layers such as
/// rate limiting. Be aware that around 250 requests will get immediately sent
/// to `svc` to process.
///
/// We return an error only when the provided `svc` errors on an HTTP request.
pub async fn parse_all<E: Send + 'static>(
    max_year: u16,
    mut client: impl HttpClient<Error = E>,
) -> Result<UsacoData, E> {
    // wrapper around our HTTP service to log strange HTTP results.
    let mut get_url = move |url: String| {
        let fut = client.get(url.parse().expect("url should be valid"));

        async move {
            let (code, html) = fut.await?;

            if !code.is_success() {
                if code == StatusCode::NOT_FOUND {
                    debug!("{url} NOT FOUND");
                } else {
                    warn!("unexpected status code {code} for url {url}");
                }
                Ok(None)
            } else {
                Ok(Some(html))
            }
        }
    };

    let mut join_set_contests = JoinSet::new();
    let mut join_set_camps = JoinSet::new();

    for season in 2012..=max_year {
        // deal with some USACO format changes causing not every year to have same
        // number of contests or divisions
        let months = if season <= 2014 {
            [
                Month::November,
                Month::December,
                Month::January,
                Month::February,
                Month::March,
                Month::Open,
            ]
            .iter()
        } else {
            [
                Month::December,
                Month::January,
                Month::February,
                Month::Open,
            ]
            .iter()
        }
        .copied();
        let divisions = if season <= 2015 {
            [Division::Bronze, Division::Silver, Division::Gold].iter()
        } else {
            [
                Division::Bronze,
                Division::Silver,
                Division::Gold,
                Division::Platinum,
            ]
            .iter()
        }
        .copied();

        for month in months {
            let year = if matches!(month, Month::November | Month::December) {
                season - 1
            } else {
                season
            };

            for division in divisions.clone() {
                let url = format!(
                    "https://usaco.org/current/data/{}{}_{}_results.html",
                    month.url_name(),
                    year % 100,
                    division.url_name(),
                );
                let req = get_url(url);

                join_set_contests.spawn(async move {
                    req.await.map(|res| {
                        res.map(|html| {
                            parse_contest_page(MonthYear { month, year }, division, &html)
                        })
                    })
                });
            }
        }

        {
            let url = format!("https://usaco.org/index.php?page=finalists{}", season % 100);
            let req = get_url(url);

            join_set_camps.spawn(async move {
                req.await
                    .map(|res| res.map(|html| parse_camp_page(season, &html)))
            });
        }
    }

    let intl_history = async {
        get_url("https://usaco.org/index.php?page=history".to_string())
            .await
            .map(|res| {
                // if we couldn't load the history page, we'll just parse the empty string and
                // return an empty result
                parse_history_page(&res.unwrap_or_default())
            })
    };

    let (contests, camps, intl_history) = tokio::join!(
        join_set_contests.join_all(),
        join_set_camps.join_all(),
        intl_history
    );
    let intl_history = intl_history?;

    let mut contests = contests
        .into_iter()
        .filter_map(|x| x.transpose())
        .collect::<Result<Vec<_>, _>>()?;
    let mut camps = camps
        .into_iter()
        .filter_map(|x| x.transpose())
        .collect::<Result<Vec<_>, _>>()?;

    contests.sort_unstable_by_key(|c| (c.time, c.division));
    camps.sort_unstable_by_key(|c| c.year);

    Ok(UsacoData {
        contests,
        camps,
        intl_history,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_month_ord() {
        // Test ordinal order of months
        assert!(Month::January < Month::February);
        assert!(Month::December > Month::November);
        assert!(Month::March <= Month::March);
    }

    #[test]
    fn test_monthyear_ord() {
        // Test ordering by year first, then by month
        let my1 = MonthYear {
            year: 2024,
            month: Month::January,
        };
        let my2 = MonthYear {
            year: 2024,
            month: Month::February,
        };
        let my3 = MonthYear {
            year: 2025,
            month: Month::January,
        };

        assert!(my1 < my2); // Same year, earlier month
        assert!(my2 < my3); // Earlier year
        assert!(my1 < my3); // Earlier year
        assert!(my3 > my2); // Later year
    }

    #[test]
    fn test_graduation_ord() {
        // Test ordering of Graduation enum variants
        let hs2024 = Graduation::HighSchool { year: 2024 };
        let hs2025 = Graduation::HighSchool { year: 2025 };
        let observer = Graduation::Observer;

        assert!(hs2024 < hs2025); // Earlier high school graduation year
        assert!(observer > hs2025); // Observer is considered "greater"
        assert!(hs2024 <= hs2024); // Same variant, same year
    }

    #[test]
    fn test_graduation_enum_order() {
        // Additional test for Graduation ordering
        let hs2024 = Graduation::HighSchool { year: 2024 };
        let observer = Graduation::Observer;

        // HighSchool variants come before Observer
        assert!(hs2024 < observer);
    }

    #[test]
    fn test_normalize_text() {
        assert_eq!(normalize_text("A   B   C"), "A B C");
        assert_eq!(normalize_text("A\tB\nC"), "A B C");
        assert_eq!(normalize_text("   A B C   "), "A B C");
        assert_eq!(normalize_text("A\u{00A0}B\u{00A0}C"), "A B C");
        assert_eq!(normalize_text(""), "");
        assert_eq!(normalize_text("   \t\n"), "");
        assert_eq!(normalize_text("Word"), "Word");
    }
}
