//! Observations: one thing that went wrong while an agent worked. Stored as
//! a note (so it is chunked, embedded and searched like one) that also
//! carries a category and a status. They never dedup: the same event saved
//! six times is six rows, and the count is the signal.

use crate::db::now;
use crate::store::{utc_day, Rag};
use crate::ws;
use anyhow::{anyhow, bail, Result};
use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ValueRef};
use rusqlite::{params, OptionalExtension};
use serde::Serialize;
use std::collections::BTreeMap;
use std::str::FromStr;

/// The `app` of an observation outside any app (`shared/observations`).
pub const SHARED: &str = "shared";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Category {
    Correction,
    Rework,
    Search,
    Skill,
    Tool,
}

impl Category {
    pub const ALL: [Category; 5] = [
        Category::Correction,
        Category::Rework,
        Category::Search,
        Category::Skill,
        Category::Tool,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Category::Correction => "correction",
            Category::Rework => "rework",
            Category::Search => "search",
            Category::Skill => "skill",
            Category::Tool => "tool",
        }
    }
}

impl FromStr for Category {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|c| c.as_str() == s)
            .ok_or_else(|| {
                let all: Vec<&str> = Self::ALL.iter().map(|c| c.as_str()).collect();
                anyhow!("unknown category {s:?} (one of: {})", all.join(", "))
            })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Open,
    Done,
    Dismissed,
}

impl Status {
    pub const ALL: [Status; 3] = [Status::Open, Status::Done, Status::Dismissed];

    pub fn as_str(self) -> &'static str {
        match self {
            Status::Open => "open",
            Status::Done => "done",
            Status::Dismissed => "dismissed",
        }
    }
}

impl FromStr for Status {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|c| c.as_str() == s)
            .ok_or_else(|| {
                let all: Vec<&str> = Self::ALL.iter().map(|c| c.as_str()).collect();
                anyhow!("unknown status {s:?} (one of: {})", all.join(", "))
            })
    }
}

impl FromSql for Category {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        value
            .as_str()?
            .parse()
            .map_err(|e: anyhow::Error| FromSqlError::Other(e.into()))
    }
}

impl FromSql for Status {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        value
            .as_str()?
            .parse()
            .map_err(|e: anyhow::Error| FromSqlError::Other(e.into()))
    }
}

/// What `done`, `dismiss` and `reopen` do to an observation. Closing takes
/// the optional resolution text; reopening clears it.
#[derive(Debug, Clone, Copy)]
pub enum Transition<'a> {
    Done(Option<&'a str>),
    Dismiss(Option<&'a str>),
    Reopen,
}

#[derive(Debug, Serialize)]
pub struct Obs {
    pub id: i64,
    /// `apps/<app>/…` → `<app>`; anything else is `shared`.
    pub app: String,
    pub ws: String,
    pub category: Category,
    pub status: Status,
    pub title: String,
    pub body: String,
    pub source: Option<String>,
    pub created: i64,
    pub resolution: Option<String>,
    pub closed_at: Option<i64>,
}

/// `None` in a field means "any".
#[derive(Debug, Default)]
pub struct ObsFilter {
    pub status: Option<Status>,
    pub category: Option<Category>,
    pub app: Option<String>,
    /// Created at or after this unix time.
    pub since: Option<i64>,
}

#[derive(Debug, Default, PartialEq, Serialize)]
pub struct ObsStats {
    pub total: usize,
    pub by_status: BTreeMap<String, usize>,
    pub by_category: BTreeMap<String, usize>,
    pub by_app: BTreeMap<String, usize>,
}

/// The cutoff a `--since` value names: `7d` and `24h` count back from
/// `now`, a `YYYY-MM-DD` date is its UTC midnight.
pub fn since_cutoff(since: &str, now: i64) -> Result<i64> {
    let back = |digits: &str, unit: i64| digits.parse::<u32>().ok().map(|n| now - n as i64 * unit);
    since
        .strip_suffix('d')
        .and_then(|n| back(n, 86_400))
        .or_else(|| since.strip_suffix('h').and_then(|n| back(n, 3_600)))
        .or_else(|| utc_midnight(since))
        .ok_or_else(|| anyhow!("bad --since {since:?} (7d, 24h or 2026-10-01)"))
}

/// `None` unless `date` is a real calendar day written `YYYY-MM-DD`: the
/// length bounds the year, and reading the result back catches 02-31.
fn utc_midnight(date: &str) -> Option<i64> {
    if date.len() != 10 {
        return None;
    }
    let mut parts = date.splitn(3, '-').map(|p| p.parse::<i64>().ok());
    let (y, m, d) = (parts.next()??, parts.next()??, parts.next()??);
    let midnight = days_from_civil(y, m, d) * 86_400;
    (utc_day(midnight) == date).then_some(midnight)
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = y - i64::from(m <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn app_name(app: &str) -> Result<String> {
    ws::normalize(app)?
        .filter(|name| !name.contains('/'))
        .ok_or_else(|| anyhow!("bad app {app:?}: a name, not a path"))
}

fn app_of(ws_path: &str) -> String {
    ws_path
        .strip_prefix("apps/")
        .and_then(|rest| rest.split('/').next())
        .unwrap_or(SHARED)
        .to_string()
}

const OBS_COLUMNS: &str = "d.id, w.path, d.category, d.status, d.title, d.body, d.source,
     d.created, d.resolution, d.closed_at
     FROM documents d JOIN workspaces w ON w.id = d.ws_id
     WHERE d.category IS NOT NULL AND d.deleted_at IS NULL";

fn obs_row(r: &rusqlite::Row) -> rusqlite::Result<Obs> {
    let ws: String = r.get(1)?;
    Ok(Obs {
        id: r.get(0)?,
        app: app_of(&ws),
        ws,
        category: r.get(2)?,
        status: r.get(3)?,
        title: r.get(4)?,
        body: r.get(5)?,
        source: r.get(6)?,
        created: r.get(7)?,
        resolution: r.get(8)?,
        closed_at: r.get(9)?,
    })
}

impl Rag {
    /// Save an observation in `app`'s observations workspace (`shared`
    /// without one), creating the workspace when missing.
    pub fn obs_add(
        &mut self,
        app: Option<&str>,
        category: Category,
        title: Option<&str>,
        text: &str,
        source: Option<&str>,
    ) -> Result<Obs> {
        let ws_path = match app.map(app_name).transpose()? {
            Some(app) if app != SHARED => format!("apps/{app}/observations"),
            _ => format!("{SHARED}/observations"),
        };
        let ws_id = self.ws_ensure(&ws_path)?;
        let id = self.add_observation(ws_id, title, text, source, category)?;
        self.obs(id)
    }

    pub fn obs(&self, id: i64) -> Result<Obs> {
        let found = self
            .conn
            .query_row(
                &format!("SELECT {OBS_COLUMNS} AND d.id = ?1"),
                [id],
                obs_row,
            )
            .optional()?;
        match found {
            Some(o) => Ok(o),
            None if self.note(id).is_ok_and(|n| n.deleted_at.is_none()) => {
                bail!("note:{id} is a note, not an observation")
            }
            None => bail!("no obs:{id}"),
        }
    }

    /// Live observations matching `filter`, newest first.
    pub fn obs_list(&self, filter: &ObsFilter) -> Result<Vec<Obs>> {
        let mut st = self.conn.prepare(&format!(
            "SELECT {OBS_COLUMNS}
               AND (?1 IS NULL OR d.status = ?1)
               AND (?2 IS NULL OR d.category = ?2)
               AND (?3 IS NULL OR d.created >= ?3)
             ORDER BY d.created DESC, d.id DESC"
        ))?;
        let rows = st
            .query_map(
                params![
                    filter.status.map(Status::as_str),
                    filter.category.map(Category::as_str),
                    filter.since
                ],
                obs_row,
            )?
            .collect::<rusqlite::Result<Vec<Obs>>>()?;
        Ok(match filter.app.as_deref().map(app_name).transpose()? {
            Some(app) => rows.into_iter().filter(|o| o.app == app).collect(),
            None => rows,
        })
    }

    /// Closing again with the same status keeps the first closing time,
    /// and the resolution unless a new one is given.
    pub fn obs_apply(&self, id: i64, transition: Transition) -> Result<Obs> {
        self.obs(id)?;
        let (status, note) = match transition {
            Transition::Done(note) => (Status::Done, note),
            Transition::Dismiss(note) => (Status::Dismissed, note),
            Transition::Reopen => (Status::Open, None),
        };
        self.conn.execute(
            "UPDATE documents SET
                 resolution = CASE WHEN ?2 = 'open' THEN NULL
                     ELSE coalesce(?3, CASE WHEN status = ?2 THEN resolution END) END,
                 closed_at = CASE WHEN ?2 = 'open' THEN NULL
                     WHEN status = ?2 THEN closed_at ELSE ?4 END,
                 status = ?2
             WHERE id = ?1",
            params![id, status.as_str(), note, now()],
        )?;
        self.obs(id)
    }

    pub fn obs_stats(&self, since: Option<i64>) -> Result<ObsStats> {
        let mut stats = ObsStats::default();
        let filter = ObsFilter {
            since,
            ..ObsFilter::default()
        };
        for o in self.obs_list(&filter)? {
            stats.total += 1;
            *stats.by_status.entry(o.status.as_str().into()).or_default() += 1;
            *stats
                .by_category
                .entry(o.category.as_str().into())
                .or_default() += 1;
            *stats.by_app.entry(o.app).or_default() += 1;
        }
        Ok(stats)
    }
}

#[cfg(test)]
#[path = "obs_tests.rs"]
mod tests;
