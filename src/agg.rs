//! Per-session aggregation into the two output tables (T06).
//!
//! `rules.md` "Outputs": `user_daily` is one row per person and session-start
//! day, `campaign_daily` is one row per credited or assisting campaign and day.
//! Rows are accumulated across sessions, so callers feed every session once and
//! write once at the end.
//!
//! Keys and labels are borrowed from the caller's input and lookup maps, so no
//! ID or category is copied per event; they stay alive through the final write.

use std::io;
use std::path::Path;

use hashbrown::HashMap;

use crate::output::{self, CampaignDaily, UserDaily};

/// The fallback campaign for a session without a qualifying click. It never
/// assists (`rules.md` "Clicks" step 7), so its channel is itself.
pub const ORGANIC: &[u8] = b"organic";

/// One person-day accumulator.
#[derive(Default)]
struct UserDay<'a> {
    sessions: i64,
    events: i64,
    revenue_cents: i64,
    /// Summed purchase cents per category; empty when the day has no purchase.
    categories: HashMap<&'a [u8], i64>,
}

impl<'a> UserDay<'a> {
    /// Highest summed purchase cents; a tie goes to the byte-wise smallest
    /// category. `unknown` competes like any other category. `None` when the
    /// person-day has no purchases.
    fn top_category(&self) -> Option<&'a [u8]> {
        let mut best: Option<(&'a [u8], i64)> = None;
        for (&category, &cents) in &self.categories {
            best = match best {
                None => Some((category, cents)),
                Some((best_category, best_cents))
                    if cents > best_cents || (cents == best_cents && category < best_category) =>
                {
                    Some((category, cents))
                }
                Some((best_category, best_cents)) => Some((best_category, best_cents)),
            };
        }
        best.map(|(category, _)| category)
    }
}

/// One campaign-day accumulator.
#[derive(Default)]
struct CampaignDay {
    sessions: i64,
    purchasing_sessions: i64,
    revenue_cents: i64,
    assisted_sessions: i64,
}

/// Accumulates both output tables. `Default` starts empty.
#[derive(Default)]
pub struct Aggregates<'a> {
    users: HashMap<(&'a [u8], i32), UserDay<'a>>,
    campaigns: HashMap<(&'a [u8], &'a [u8], i32), CampaignDay>,
}

impl<'a> Aggregates<'a> {
    /// Folds one session into both tables.
    ///
    /// * `user` is the person's name and `day` the session start's UTC day.
    /// * `events` is the session's surviving event count.
    /// * `purchases` yields `(category, price_cents)` per purchase; `unknown` is
    ///   an ordinary category.
    /// * `credit` is the session's `(campaign_id, channel)`, `organic` included.
    /// * `assists` yields each assisting `(campaign_id, channel)` once, as
    ///   provided by the caller; `organic` is ignored defensively.
    pub fn add_session(
        &mut self,
        user: &'a [u8],
        day: i32,
        events: i64,
        purchases: impl Iterator<Item = (&'a [u8], i64)>,
        credit: (&'a [u8], &'a [u8]),
        assists: impl Iterator<Item = (&'a [u8], &'a [u8])>,
    ) {
        let mut revenue_cents = 0i64;
        let mut purchasing = false;

        let user_day = self.users.entry((user, day)).or_default();
        user_day.sessions += 1;
        user_day.events += events;
        for (category, cents) in purchases {
            purchasing = true;
            revenue_cents += cents;
            *user_day.categories.entry(category).or_insert(0) += cents;
        }
        user_day.revenue_cents += revenue_cents;

        let credited = self.campaigns.entry((credit.0, credit.1, day)).or_default();
        credited.sessions += 1;
        if purchasing {
            credited.purchasing_sessions += 1;
        }
        credited.revenue_cents += revenue_cents;

        // An assist-only campaign still gets a row, with zero credited columns.
        for (campaign, channel) in assists {
            if campaign == ORGANIC {
                continue;
            }
            self.campaigns
                .entry((campaign, channel, day))
                .or_default()
                .assisted_sessions += 1;
        }
    }

    /// Writes `user_daily` and `campaign_daily` under `out`, creating both
    /// folders. Row order follows the maps and is not significant
    /// (`rules.md` "Outputs"); each table's rows live only for its write.
    pub fn write(&self, out: &Path) -> io::Result<()> {
        {
            let user_rows: Vec<UserDaily<'a>> = self
                .users
                .iter()
                .map(|(&(user_id, day), user_day)| UserDaily {
                    user_id,
                    day,
                    sessions: user_day.sessions,
                    events: user_day.events,
                    revenue_cents: user_day.revenue_cents,
                    top_category: user_day.top_category(),
                })
                .collect();
            output::write_user_daily(&out.join("user_daily"), &user_rows)?;
        }

        {
            let campaign_rows: Vec<CampaignDaily<'a>> = self
                .campaigns
                .iter()
                .map(
                    |(&(campaign_id, channel, day), campaign_day)| CampaignDaily {
                        campaign_id,
                        channel,
                        day,
                        sessions: campaign_day.sessions,
                        purchasing_sessions: campaign_day.purchasing_sessions,
                        revenue_cents: campaign_day.revenue_cents,
                        assisted_sessions: campaign_day.assisted_sessions,
                    },
                )
                .collect();
            output::write_campaign_daily(&out.join("campaign_daily"), &campaign_rows)?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::path::PathBuf;

    use parquet::file::reader::{FileReader, SerializedFileReader};
    use parquet::record::Field;

    /// Fixture window: Oct 1 to Oct 15 2026. Oct 3 is day 20729.
    const DAY: i32 = 20729;

    /// Expected `user_daily` row, in schema order.
    type UserRow = (String, i32, i64, i64, i64, Option<String>);
    /// Expected `campaign_daily` row, in schema order.
    type CampaignRow = (String, String, i32, i64, i64, i64, i64);

    fn scratch_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("shoptimizer-t06-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn string(field: &Field) -> String {
        match field {
            Field::Str(value) => value.clone(),
            Field::Bytes(value) => String::from_utf8(value.data().to_vec()).unwrap(),
            other => panic!("expected a string, got {other:?}"),
        }
    }

    fn integer(field: &Field) -> i64 {
        match field {
            Field::Date(value) | Field::Int(value) => i64::from(*value),
            Field::Long(value) => *value,
            other => panic!("expected an integer, got {other:?}"),
        }
    }

    fn read_user_daily(dir: &Path) -> Vec<UserRow> {
        let file = File::open(dir.join("user_daily").join("part-0.parquet")).unwrap();
        let reader = SerializedFileReader::new(file).unwrap();
        let mut rows: Vec<UserRow> = reader
            .get_row_iter(None)
            .unwrap()
            .map(|row| {
                let row = row.unwrap();
                let fields: Vec<&Field> = row.get_column_iter().map(|(_, field)| field).collect();
                let top_category = match fields[5] {
                    Field::Null => None,
                    other => Some(string(other)),
                };
                (
                    string(fields[0]),
                    integer(fields[1]) as i32,
                    integer(fields[2]),
                    integer(fields[3]),
                    integer(fields[4]),
                    top_category,
                )
            })
            .collect();
        // `write` leaves rows in map order; compare as sets.
        rows.sort_unstable();
        rows
    }

    fn read_campaign_daily(dir: &Path) -> Vec<CampaignRow> {
        let file = File::open(dir.join("campaign_daily").join("part-0.parquet")).unwrap();
        let reader = SerializedFileReader::new(file).unwrap();
        let mut rows: Vec<CampaignRow> = reader
            .get_row_iter(None)
            .unwrap()
            .map(|row| {
                let row = row.unwrap();
                let fields: Vec<&Field> = row.get_column_iter().map(|(_, field)| field).collect();
                (
                    string(fields[0]),
                    string(fields[1]),
                    integer(fields[2]) as i32,
                    integer(fields[3]),
                    integer(fields[4]),
                    integer(fields[5]),
                    integer(fields[6]),
                )
            })
            .collect();
        // `write` leaves rows in map order; compare as sets.
        rows.sort_unstable();
        rows
    }

    /// E8: purchases of `p1` (shoes, 500) and `p2` (books, 500) tie, so the
    /// byte-wise smallest category `books` wins, and both tables count the
    /// revenue once.
    #[test]
    fn tie_goes_to_the_byte_wise_smallest_category() {
        let dir = scratch_dir("e8");
        let mut aggregates = Aggregates::default();
        aggregates.add_session(
            b"u1",
            DAY,
            2,
            [(b"shoes".as_slice(), 500), (b"books".as_slice(), 500)].into_iter(),
            (ORGANIC, ORGANIC),
            std::iter::empty(),
        );
        aggregates.write(&dir).unwrap();

        assert_eq!(
            read_user_daily(&dir),
            vec![("u1".to_string(), DAY, 1, 2, 1000, Some("books".to_string()))]
        );
        assert_eq!(
            read_campaign_daily(&dir),
            vec![(
                "organic".to_string(),
                "organic".to_string(),
                DAY,
                1,
                1,
                1000,
                0
            )]
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// E9: a product missing from `products.csv` is `unknown`, and it can win:
    /// `unknown` 2000 beats `shoes` 1500.
    #[test]
    fn unknown_competes_like_any_category() {
        let dir = scratch_dir("e9");
        let mut aggregates = Aggregates::default();
        aggregates.add_session(
            b"u1",
            DAY,
            2,
            [(b"unknown".as_slice(), 2000), (b"shoes".as_slice(), 1500)].into_iter(),
            (ORGANIC, ORGANIC),
            std::iter::empty(),
        );
        aggregates.write(&dir).unwrap();

        assert_eq!(
            read_user_daily(&dir),
            vec![(
                "u1".to_string(),
                DAY,
                1,
                2,
                3500,
                Some("unknown".to_string())
            )]
        );
        assert_eq!(
            read_campaign_daily(&dir),
            vec![(
                "organic".to_string(),
                "organic".to_string(),
                DAY,
                1,
                1,
                3500,
                0
            )]
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A11: every campaign clicked in the 7 days up to the start assists once,
    /// including the crediting one. A campaign that only assisted gets a row
    /// with zero credited columns; `organic` never assists.
    #[test]
    fn assists_include_the_crediting_campaign_and_stand_alone() {
        let dir = scratch_dir("a11");
        let mut aggregates = Aggregates::default();
        aggregates.add_session(
            b"u1",
            DAY,
            1,
            [(b"shoes".as_slice(), 1000)].into_iter(),
            (b"c_2", b"social"),
            [
                (b"c_1".as_slice(), b"search".as_slice()),
                (b"c_2".as_slice(), b"social".as_slice()),
            ]
            .into_iter(),
        );
        aggregates.add_session(
            b"u2",
            DAY,
            1,
            std::iter::empty(),
            (ORGANIC, ORGANIC),
            // A click 7 days + 1 ms before does not assist; organic never does.
            [(ORGANIC, ORGANIC)].into_iter(),
        );
        aggregates.write(&dir).unwrap();

        assert_eq!(
            read_user_daily(&dir),
            vec![
                ("u1".to_string(), DAY, 1, 1, 1000, Some("shoes".to_string())),
                ("u2".to_string(), DAY, 1, 1, 0, None),
            ]
        );
        assert_eq!(
            read_campaign_daily(&dir),
            vec![
                ("c_1".to_string(), "search".to_string(), DAY, 0, 0, 0, 1),
                ("c_2".to_string(), "social".to_string(), DAY, 1, 1, 1000, 1),
                (
                    "organic".to_string(),
                    "organic".to_string(),
                    DAY,
                    1,
                    0,
                    0,
                    0
                ),
            ]
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Sessions on different days accumulate into separate rows; sessions on
    /// the same person-day add up. `purchasing_sessions` counts only credited
    /// sessions with at least one purchase.
    #[test]
    fn aggregates_across_sessions_and_days() {
        let dir = scratch_dir("cross-session");
        let mut aggregates = Aggregates::default();
        // Day 1: two sessions, one buying and one browsing.
        aggregates.add_session(
            b"u1",
            DAY,
            3,
            [(b"shoes".as_slice(), 400)].into_iter(),
            (b"c_1", b"search"),
            [
                (b"c_1".as_slice(), b"search".as_slice()),
                (b"c_2".as_slice(), b"social".as_slice()),
            ]
            .into_iter(),
        );
        aggregates.add_session(
            b"u1",
            DAY,
            1,
            std::iter::empty(),
            (b"c_1", b"search"),
            [(b"c_1".as_slice(), b"search".as_slice())].into_iter(),
        );
        // Next day: one buying session credited elsewhere.
        aggregates.add_session(
            b"u1",
            DAY + 1,
            2,
            [(b"books".as_slice(), 750), (b"books".as_slice(), 250)].into_iter(),
            (ORGANIC, ORGANIC),
            std::iter::empty(),
        );
        aggregates.write(&dir).unwrap();

        assert_eq!(
            read_user_daily(&dir),
            vec![
                ("u1".to_string(), DAY, 2, 4, 400, Some("shoes".to_string())),
                (
                    "u1".to_string(),
                    DAY + 1,
                    1,
                    2,
                    1000,
                    Some("books".to_string())
                ),
            ]
        );
        assert_eq!(
            read_campaign_daily(&dir),
            vec![
                ("c_1".to_string(), "search".to_string(), DAY, 2, 1, 400, 2),
                ("c_2".to_string(), "social".to_string(), DAY, 0, 0, 0, 1),
                (
                    "organic".to_string(),
                    "organic".to_string(),
                    DAY + 1,
                    1,
                    1,
                    1000,
                    0
                ),
            ]
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
