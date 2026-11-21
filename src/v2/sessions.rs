//! Session kernel for one person bucket: sessionize, credit, assists, and fold
//! into `user_daily` rows and dense `campaign_daily` counters (D15, D16, O10).

use super::types::{Ck, Ctx, Ev, UserRow, LOOKBACK_MS, SESSION_GAP_MS};

/// `camp` counter columns.
pub const SESSIONS: usize = 0;
pub const PURCHASING: usize = 1;
pub const REVENUE: usize = 2;
pub const ASSISTED: usize = 3;

/// Per-worker accumulator.
pub struct Acc {
    pub users: Vec<UserRow>,
    /// `(campaign index or organic) * n_days + day index`.
    pub camp: Vec<[i64; 4]>,
    n_days: usize,
    organic: usize,
    // Sliding-window assist state (D15).
    counts: Vec<u32>,
    pos: Vec<u32>,
    active: Vec<u16>,
    // top_category state for the current person-day.
    cat_sum: Vec<i64>,
    cat_touched: Vec<u16>,
}

impl Acc {
    pub fn new(ctx: &Ctx) -> Self {
        let n_campaigns = ctx.campaign_ids.len();
        Acc {
            users: Vec::new(),
            camp: vec![[0; 4]; (n_campaigns + 1) * ctx.n_days],
            n_days: ctx.n_days,
            organic: n_campaigns,
            counts: vec![0; n_campaigns],
            pos: vec![0; n_campaigns],
            active: Vec::new(),
            cat_sum: vec![0; ctx.categories.len()],
            cat_touched: Vec::new(),
        }
    }

    pub fn merge(mut self, mut other: Acc) -> Acc {
        if other.users.len() > self.users.len() {
            std::mem::swap(&mut self.users, &mut other.users);
        }
        self.users.append(&mut other.users);
        for (a, b) in self.camp.iter_mut().zip(&other.camp) {
            for k in 0..4 {
                a[k] += b[k];
            }
        }
        self
    }

    #[inline]
    fn bump(&mut self, c: u16) {
        let ci = c as usize;
        self.counts[ci] += 1;
        if self.counts[ci] == 1 {
            self.pos[ci] = self.active.len() as u32;
            self.active.push(c);
        }
    }

    #[inline]
    fn unbump(&mut self, c: u16) {
        let ci = c as usize;
        self.counts[ci] -= 1;
        if self.counts[ci] == 0 {
            let p = self.pos[ci] as usize;
            self.active.swap_remove(p);
            if p < self.active.len() {
                self.pos[self.active[p] as usize] = p as u32;
            }
        }
    }

    fn top_category(&mut self) -> Option<u16> {
        let mut best: Option<(u16, i64)> = None;
        for &c in &self.cat_touched {
            let cents = self.cat_sum[c as usize];
            best = match best {
                Some((bc, bs)) if bs > cents || (bs == cents && bc < c) => Some((bc, bs)),
                _ => Some((c, cents)),
            };
            self.cat_sum[c as usize] = 0;
        }
        self.cat_touched.clear();
        best.map(|(c, _)| c)
    }

    /// One person's events (sorted by ts) and clicks (sorted by ts, click_id).
    fn person(&mut self, person: u128, evs: &[Ev], cks: &[Ck], ctx: &Ctx) {
        let (mut lo, mut hi) = (0usize, 0usize);
        let mut row: Option<UserRow> = None;
        let mut s = 0;
        while s < evs.len() {
            let start = evs[s].ts;
            let mut e = s + 1;
            let mut prev = start;
            // Gaps between consecutive distinct timestamps; equal ts share a session.
            while e < evs.len() && evs[e].ts - prev <= SESSION_GAP_MS {
                prev = evs[e].ts;
                e += 1;
            }

            // Click window [start - 7 d, start] in click-relative ms.
            let upper = start as i64 + LOOKBACK_MS;
            let lower = start as i64;
            while hi < cks.len() && (cks[hi].ts as i64) <= upper {
                self.bump(cks[hi].campaign);
                hi += 1;
            }
            while lo < hi && (cks[lo].ts as i64) < lower {
                self.unbump(cks[lo].campaign);
                lo += 1;
            }
            let credit = if hi > lo {
                let latest = cks[hi - 1].ts;
                let first = lo + cks[lo..hi].partition_point(|c| c.ts < latest);
                cks[first].campaign as usize
            } else {
                self.organic
            };

            let day = crate::ts::day_of(ctx.window.start + start as i64);
            match row {
                Some(r) if r.day == day => {}
                _ => {
                    if let Some(mut r) = row.take() {
                        r.top_cat = self.top_category();
                        self.users.push(r);
                    }
                    row = Some(UserRow {
                        person,
                        day,
                        sessions: 0,
                        events: 0,
                        revenue: 0,
                        top_cat: None,
                    });
                }
            }

            let mut revenue = 0i64;
            let mut purchasing = false;
            for ev in &evs[s..e] {
                if ev.purchase {
                    purchasing = true;
                    revenue += ev.price as i64;
                    let c = ev.cat as usize;
                    if self.cat_sum[c] == 0 {
                        self.cat_touched.push(ev.cat);
                    }
                    self.cat_sum[c] += ev.price as i64;
                }
            }
            let r = row.as_mut().unwrap();
            r.sessions += 1;
            r.events += (e - s) as i64;
            r.revenue += revenue;

            let d = (day - ctx.day0) as usize;
            let cell = &mut self.camp[credit * self.n_days + d];
            cell[SESSIONS] += 1;
            cell[PURCHASING] += purchasing as i64;
            cell[REVENUE] += revenue;
            for &a in &self.active {
                self.camp[a as usize * self.n_days + d][ASSISTED] += 1;
            }
            s = e;
        }
        if let Some(mut r) = row {
            r.top_cat = self.top_category();
            self.users.push(r);
        }
        for &c in &self.active {
            self.counts[c as usize] = 0;
        }
        self.active.clear();
    }

    /// Every person in one bucket. Sorts both slices in place.
    pub fn run_bucket(&mut self, evs: &mut [Ev], cks: &mut [Ck], ctx: &Ctx) {
        evs.sort_unstable_by_key(|e| (e.person, e.ts));
        cks.sort_unstable_by_key(|c| (c.person, c.ts, c.click_id));
        let (mut i, mut ci) = (0usize, 0usize);
        while i < evs.len() {
            let person = evs[i].person;
            let mut j = i + 1;
            while j < evs.len() && evs[j].person == person {
                j += 1;
            }
            while ci < cks.len() && cks[ci].person < person {
                ci += 1;
            }
            let mut cj = ci;
            while cj < cks.len() && cks[cj].person == person {
                cj += 1;
            }
            self.person(person, &evs[i..j], &cks[ci..cj], ctx);
            i = j;
            ci = cj;
        }
    }
}
