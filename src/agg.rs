//! Output aggregation helpers (rules.md "Outputs"): folds finished, attributed
//! sessions into one row per (person, day) and one row per (campaign, day).

use std::collections::HashMap;

use crate::output::{CampaignDaily, UserDaily};

/// One finished session, already attributed.
pub struct SessionSummary<'a> {
    pub person: &'a [u8],
    pub day: i32,
    pub events: i64,
    pub purchases: Vec<(&'a [u8], i64)>,
    pub credited: &'a [u8],
    pub assists: Vec<&'a [u8]>,
}

#[derive(Default)]
struct UserAcc<'a> {
    sessions: i64,
    events: i64,
    revenue_cents: i64,
    /// category -> summed purchase cents over all of the person-day's sessions.
    cents_by_category: HashMap<&'a [u8], i64>,
}

#[derive(Default)]
struct CampaignAcc {
    sessions: i64,
    purchasing_sessions: i64,
    revenue_cents: i64,
    assisted_sessions: i64,
}

#[derive(Default)]
pub struct Aggregator<'a> {
    users: HashMap<(&'a [u8], i32), UserAcc<'a>>,
    campaigns: HashMap<(&'a [u8], i32), CampaignAcc>,
}

impl<'a> Aggregator<'a> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_session(&mut self, s: &SessionSummary<'a>) {
        let revenue: i64 = s.purchases.iter().map(|&(_, cents)| cents).sum();

        let u = self.users.entry((s.person, s.day)).or_default();
        u.sessions += 1;
        u.events += s.events;
        u.revenue_cents += revenue;
        for &(category, cents) in &s.purchases {
            *u.cents_by_category.entry(category).or_insert(0) += cents;
        }

        let c = self.campaigns.entry((s.credited, s.day)).or_default();
        c.sessions += 1;
        c.purchasing_sessions += (!s.purchases.is_empty()) as i64;
        c.revenue_cents += revenue;

        for &campaign in &s.assists {
            self.campaigns.entry((campaign, s.day)).or_default().assisted_sessions += 1;
        }
    }

    /// One row per (person, day) with at least one session, sorted by (person, day).
    pub fn user_daily(&self) -> Vec<UserDaily<'a>> {
        let mut rows: Vec<UserDaily<'a>> = self
            .users
            .iter()
            .map(|(&(person, day), u)| UserDaily {
                user_id: person,
                day,
                sessions: u.sessions,
                events: u.events,
                revenue_cents: u.revenue_cents,
                top_category: top_category(&u.cents_by_category),
            })
            .collect();
        rows.sort_by(|a, b| (a.user_id, a.day).cmp(&(b.user_id, b.day)));
        rows
    }

    /// One row per (campaign_id, day) credited or assisted, sorted by (campaign_id, day).
    /// `channel_of` is never called for b"organic", whose channel is b"organic".
    pub fn campaign_daily(&self, channel_of: impl Fn(&'a [u8]) -> &'a [u8]) -> Vec<CampaignDaily<'a>> {
        let mut rows: Vec<CampaignDaily<'a>> = self
            .campaigns
            .iter()
            .map(|(&(campaign_id, day), c)| CampaignDaily {
                campaign_id,
                channel: if campaign_id == b"organic" { b"organic" } else { channel_of(campaign_id) },
                day,
                sessions: c.sessions,
                purchasing_sessions: c.purchasing_sessions,
                revenue_cents: c.revenue_cents,
                assisted_sessions: c.assisted_sessions,
            })
            .collect();
        rows.sort_by(|a, b| (a.campaign_id, a.day).cmp(&(b.campaign_id, b.day)));
        rows
    }
}

/// Highest summed category cents; ties go to the byte-wise smallest category.
/// None when the person-day had no purchases.
fn top_category<'a>(cents_by_category: &HashMap<&'a [u8], i64>) -> Option<&'a [u8]> {
    let mut best: Option<(&'a [u8], i64)> = None;
    for (&category, &cents) in cents_by_category {
        best = match best {
            None => Some((category, cents)),
            Some((bc, bv)) if cents > bv || (cents == bv && category < bc) => Some((category, cents)),
            keep => keep,
        };
    }
    best.map(|(category, _)| category)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session<'a>(
        person: &'a [u8],
        day: i32,
        events: i64,
        purchases: Vec<(&'a [u8], i64)>,
        credited: &'a [u8],
        assists: Vec<&'a [u8]>,
    ) -> SessionSummary<'a> {
        SessionSummary { person, day, events, purchases, credited, assists }
    }

    fn channel(c: &[u8]) -> &[u8] {
        match c {
            b"c_1" => b"email",
            b"c_2" => b"search",
            _ => panic!("unexpected channel lookup"),
        }
    }

    #[test]
    fn e8_tie_goes_to_smallest_category() {
        let mut agg = Aggregator::new();
        agg.add_session(&session(b"u1", 10, 3, vec![(b"shoes", 500), (b"books", 500)], b"organic", vec![]));
        let rows = agg.user_daily();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].top_category, Some(&b"books"[..]));
        assert_eq!(rows[0].revenue_cents, 1000);
    }

    #[test]
    fn e9_unknown_competes_and_can_win() {
        let mut agg = Aggregator::new();
        agg.add_session(&session(b"u1", 10, 2, vec![(b"unknown", 2000), (b"shoes", 1500)], b"organic", vec![]));
        assert_eq!(agg.user_daily()[0].top_category, Some(&b"unknown"[..]));
    }

    #[test]
    fn sums_across_sessions_on_same_day_before_choosing() {
        let mut agg = Aggregator::new();
        // Session 2 alone would pick b_cat (900 > 500); summed over the day, a_cat (1000 > 900) wins.
        agg.add_session(&session(b"u1", 10, 1, vec![(b"a_cat", 500)], b"organic", vec![]));
        agg.add_session(&session(b"u1", 10, 2, vec![(b"a_cat", 500), (b"b_cat", 900)], b"organic", vec![]));
        let rows = agg.user_daily();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].sessions, 2);
        assert_eq!(rows[0].events, 3);
        assert_eq!(rows[0].revenue_cents, 1900);
        assert_eq!(rows[0].top_category, Some(&b"a_cat"[..]));
    }

    #[test]
    fn no_purchases_gives_none_and_zero_revenue() {
        let mut agg = Aggregator::new();
        agg.add_session(&session(b"u1", 10, 4, vec![], b"organic", vec![]));
        let rows = agg.user_daily();
        assert_eq!(rows[0].top_category, None);
        assert_eq!(rows[0].revenue_cents, 0);
        assert_eq!(rows[0].sessions, 1);
        assert_eq!(rows[0].events, 4);
    }

    #[test]
    fn a11_credited_and_assisted_campaigns() {
        let mut agg = Aggregator::new();
        agg.add_session(&session(b"u1", 10, 2, vec![(b"shoes", 1000)], b"c_2", vec![b"c_1", b"c_2"]));
        let rows = agg.campaign_daily(channel);
        assert_eq!(rows.len(), 2);
        let c1 = rows.iter().find(|r| r.campaign_id == b"c_1").unwrap();
        assert_eq!((c1.channel, c1.day), (&b"email"[..], 10));
        assert_eq!((c1.sessions, c1.purchasing_sessions, c1.revenue_cents, c1.assisted_sessions), (0, 0, 0, 1));
        let c2 = rows.iter().find(|r| r.campaign_id == b"c_2").unwrap();
        assert_eq!(c2.channel, &b"search"[..]);
        assert_eq!((c2.sessions, c2.purchasing_sessions, c2.revenue_cents, c2.assisted_sessions), (1, 1, 1000, 1));
    }

    #[test]
    fn organic_session_row_has_organic_channel_and_no_assists() {
        let mut agg = Aggregator::new();
        agg.add_session(&session(b"u1", 10, 1, vec![], b"organic", vec![]));
        // channel_of must not be called for organic: it panics on anything else.
        let rows = agg.campaign_daily(|c| {
            assert_ne!(c, b"organic");
            channel(c)
        });
        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert_eq!(r.campaign_id, &b"organic"[..]);
        assert_eq!(r.channel, &b"organic"[..]);
        assert_eq!((r.sessions, r.purchasing_sessions, r.revenue_cents, r.assisted_sessions), (1, 0, 0, 0));
    }

    #[test]
    fn two_sessions_on_different_days_make_separate_rows() {
        let mut agg = Aggregator::new();
        agg.add_session(&session(b"u1", 10, 1, vec![(b"shoes", 100)], b"c_1", vec![]));
        agg.add_session(&session(b"u1", 11, 1, vec![], b"c_1", vec![]));
        let users = agg.user_daily();
        assert_eq!(users.len(), 2);
        assert_eq!((users[0].day, users[0].revenue_cents), (10, 100));
        assert_eq!((users[1].day, users[1].revenue_cents, users[1].top_category), (11, 0, None));
        let camps = agg.campaign_daily(channel);
        assert_eq!(camps.len(), 2);
        assert_eq!((camps[0].day, camps[0].sessions, camps[0].purchasing_sessions), (10, 1, 1));
        assert_eq!((camps[1].day, camps[1].sessions, camps[1].purchasing_sessions), (11, 1, 0));
    }
}
