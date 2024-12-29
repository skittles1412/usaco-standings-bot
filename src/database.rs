use poise::serenity_prelude as serenity;
use regex::Regex;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serenity::UserId;
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};
use tracing::error;
use usaco_standings_scraper::{
    CampParticipant, ContestParticipant, Division, Graduation, IntlHistory, IntlParticipant,
    MonthYear, UsacoData,
};

/// A (name, country, graduation year) tuple that is a best effort to identify
/// people across USACO monthly results.
#[derive(Debug, Clone, Ord, PartialOrd, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct ParticipantId {
    pub name: String,
    pub graduation: Graduation,
    pub country: String,
}

impl From<ContestParticipant> for ParticipantId {
    fn from(value: ContestParticipant) -> Self {
        Self {
            name: value.name,
            country: value.country,
            graduation: value.graduation,
        }
    }
}

impl From<CampParticipant> for ParticipantId {
    fn from(value: CampParticipant) -> Self {
        Self {
            name: value.name,
            country: "USA".to_string(),
            graduation: Graduation::HighSchool {
                year: value.graduation_year,
            },
        }
    }
}

/// The record of a contest for a specific participant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParticipantContestRecord {
    pub contest_time: MonthYear,
    pub division: Division,
    pub score: u16,
}

/// The record of a USACO camp for a specific participant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParticipantCampRecord {
    pub camp_year: u16,
}

/// The contests and camp data associated with a specific participant (based on
/// their id).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Participant {
    pub id: ParticipantId,
    pub contests: Vec<ParticipantContestRecord>,
    pub camps: Vec<ParticipantCampRecord>,
}

/// Stores USACO data and answers queries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsacoDb {
    pub participants: Vec<Participant>,
    intl_history: IntlHistory,
}

/// Result from querying a specific name.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NameQueryResult {
    /// Contest and camp records for this name.
    pub participants: Vec<Participant>,
    /// IOI results for this name.
    pub ioi: Vec<IntlParticipant>,
    /// EGOI results for this name.
    pub egoi: Vec<IntlParticipant>,
}

impl UsacoDb {
    /// Returns results under a specifc name. Currently, this just does a
    /// case-insensitive lookup with some normalization to get rid of duplicate
    /// whitespace.
    ///
    /// Records within each person are returned in chronological order. People
    /// are returned in order of graduation year and then country.
    ///
    /// We ignore the preferred names (the ones in parentheses) listed on the
    /// USACO camp / history pages.
    pub fn query_name(&self, name: &str) -> NameQueryResult {
        // case-insensitive search + ignore duplicate whitespace
        let name = name
            .to_lowercase()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");

        // the database is currently ~20k people and growing very slowly. also this
        // bot's usage is relatively small, so brute force should most definitely be ok.
        let mut res = NameQueryResult {
            participants: self
                .participants
                .iter()
                .filter(|p| p.id.name.to_lowercase() == name)
                .cloned()
                .collect(),
            ioi: self
                .intl_history
                .ioi
                .iter()
                .filter(|p| p.name.to_lowercase() == name)
                .cloned()
                .collect(),
            egoi: self
                .intl_history
                .egoi
                .iter()
                .filter(|p| p.name.to_lowercase() == name)
                .cloned()
                .collect(),
        };

        res.participants
            .sort_unstable_by(|p1, p2| p1.id.cmp(&p2.id));

        for p in &mut res.participants {
            p.camps.sort_unstable_by_key(|c| c.camp_year);
            p.contests
                .sort_unstable_by_key(|c| (c.contest_time, c.division));
        }

        res.ioi.sort_unstable_by_key(|c| c.year);
        res.egoi.sort_unstable_by_key(|c| c.year);

        res
    }

    /// Number of USACO people we know
    pub fn people_count(&self) -> usize {
        self.participants.len()
    }

    /// Number of contest records we know
    pub fn contest_count(&self) -> usize {
        self.participants.iter().map(|p| p.contests.len()).sum()
    }

    /// Number of camp records we know
    pub fn camp_count(&self) -> usize {
        self.participants.iter().map(|p| p.camps.len()).sum()
    }

    /// Number of IOI people we know
    pub fn ioi_people_count(&self) -> usize {
        self.intl_history
            .ioi
            .iter()
            .map(|p| &p.name)
            .collect::<HashSet<_>>()
            .len()
    }

    /// Number of IOI contest records we know
    pub fn ioi_records_count(&self) -> usize {
        self.intl_history.ioi.len()
    }

    /// Number of EGOI people we know
    pub fn egoi_people_count(&self) -> usize {
        self.intl_history
            .egoi
            .iter()
            .map(|p| &p.name)
            .collect::<HashSet<_>>()
            .len()
    }

    /// Number of EGOI contest records we know
    pub fn egoi_records_count(&self) -> usize {
        self.intl_history.egoi.len()
    }
}

impl Default for UsacoDb {
    fn default() -> Self {
        Self {
            participants: vec![],
            intl_history: IntlHistory {
                ioi: vec![],
                egoi: vec![],
            },
        }
    }
}

impl From<UsacoData> for UsacoDb {
    fn from(mut value: UsacoData) -> Self {
        // deal with the preferred names that are in parentheses
        let re = Regex::new(r#"\(.+\) "#).unwrap();

        let mut participants = HashMap::new();

        for contest in value.contests {
            for p in contest.participants {
                let id = ParticipantId::from(p.clone());

                participants
                    .entry(id.clone())
                    .or_insert_with(|| Participant {
                        id,
                        contests: vec![],
                        camps: vec![],
                    })
                    .contests
                    .push(ParticipantContestRecord {
                        contest_time: contest.time,
                        division: contest.division,
                        score: p.score,
                    });
            }
        }

        for camp in value.camps {
            for p in camp.participants {
                let id = ParticipantId::from(p.clone());

                participants
                    .entry(id.clone())
                    .or_insert_with(|| Participant {
                        id,
                        contests: vec![],
                        camps: vec![],
                    })
                    .camps
                    .push(ParticipantCampRecord {
                        camp_year: camp.year,
                    });
            }
        }

        for comp in [&mut value.intl_history.ioi, &mut value.intl_history.egoi] {
            for participant in comp {
                participant.name = re.replace(&participant.name, "").to_string();
            }
        }

        Self {
            participants: participants.into_values().collect(),
            intl_history: value.intl_history,
        }
    }
}

/// Various statistics about the bot to be preserved across runs.
#[derive(Clone, Serialize, Deserialize, Default)]
pub struct AppStats {
    /// Set of all users that have used /search at least one.
    #[serde(default)]
    pub users_queried: HashSet<UserId>,
    /// Amount of /search requests this bot has responded to.
    #[serde(default)]
    pub query_count: u32,
}

/// The data persisted by this bot.
pub struct StoreData {
    pub db: UsacoDb,
    pub stats: AppStats,
}

/// A very simple database that saves and loads from the filesystem.
pub struct FileStore {
    path: PathBuf,
}

impl FileStore {
    /// Creates a new file store that saves and loads its data from the given
    /// `path`. `path` should point to a folder.
    pub fn new_path(path: PathBuf) -> Self {
        Self { path }
    }

    /// Attempts to load data from the path. Default values will be returned if
    /// data fails to load.
    pub async fn load(&self) -> StoreData {
        async fn load<T: DeserializeOwned + Default>(path: impl AsRef<Path>) -> T {
            async {
                let data = tokio::fs::read_to_string(path.as_ref()).await?;

                Ok(serde_json::from_str::<T>(&data)?)
            }
            .await
            .unwrap_or_else(|e: anyhow::Error| {
                error!("failed to load data from path {:?} {e:?}", path.as_ref());
                Default::default()
            })
        }

        let (db, stats) = tokio::join!(
            load(self.path.join("usaco-db.json")),
            load(self.path.join("stats.json"))
        );

        StoreData { db, stats }
    }

    /// Saves `db`. We require a mutable reference to prevent racing
    /// the file system.
    pub async fn save_db(&mut self, db: &UsacoDb) -> anyhow::Result<()> {
        tokio::fs::write(self.path.join("usaco-db.json"), serde_json::to_string(&db)?).await?;

        Ok(())
    }

    /// Saves `stats`. We require a mutable reference to prevent racing
    /// the file system.
    pub async fn save_stats(&mut self, stats: &AppStats) -> anyhow::Result<()> {
        tokio::fs::write(self.path.join("stats.json"), serde_json::to_string(&stats)?).await?;

        Ok(())
    }
}
