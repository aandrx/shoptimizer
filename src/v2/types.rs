//! Shared V2 records and lookup context (D13, D17).

use hashbrown::HashMap;

use crate::csv;
use crate::id;
use crate::ts::Window;

pub const SESSION_GAP_MS: u32 = 30 * 60 * 1_000;
pub const LATE_MS: i64 = 24 * 60 * 60 * 1_000;
pub const LOOKBACK_MS: i64 = 7 * 24 * 60 * 60 * 1_000;

/// `EvRaw.flags`: the copy passed every per-copy check (rules.md Events 3-4).
pub const VALID: u8 = 1;
/// `EvRaw.flags`: the copy is a purchase.
pub const PURCHASE: u8 = 2;

/// One parsed event copy, before dedup. Invalid copies only carry `id`,
/// `ingest_s` and `flags` (O4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EvRaw {
    pub id: u128,
    pub user: u128,
    /// Exact `ingest_ts` in seconds since `window.start`.
    pub ingest_s: i32,
    /// Event `ts` in ms since `window.start`.
    pub ts: u32,
    pub price: u32,
    pub cat: u16,
    pub flags: u8,
}

/// A surviving event, owned by its person.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ev {
    pub person: u128,
    pub ts: u32,
    pub price: u32,
    pub cat: u16,
    pub purchase: bool,
}

/// A valid click, owned by its person.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ck {
    pub person: u128,
    pub click_id: u128,
    /// ms since `window.start - 7 d`.
    pub ts: u32,
    pub campaign: u16,
}

/// One `user_daily` row before string materialization.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UserRow {
    pub person: u128,
    pub day: i32,
    pub sessions: i64,
    pub events: i64,
    pub revenue: i64,
    pub top_cat: Option<u16>,
}

pub enum Parsed<T> {
    Skip,
    Overflow,
    Rec(T),
}

/// Lookups shared by every worker.
pub struct Ctx<'a> {
    pub window: Window,
    /// `window.start - 7 d`, the origin of `Ck.ts`.
    pub click_base: i64,
    /// Packed product ID -> category index.
    pub products: HashMap<u128, u16>,
    /// Category names, byte-wise sorted; the index is the category ID (D17).
    pub categories: Vec<&'a [u8]>,
    pub unknown_cat: u16,
    /// Packed campaign ID -> campaign index.
    pub campaigns: HashMap<u128, u16>,
    pub campaign_ids: Vec<&'a [u8]>,
    pub channels: Vec<&'a [u8]>,
    /// UTC day of `window.start`, and the number of days a session can start on.
    pub day0: i32,
    pub n_days: usize,
}

const UNKNOWN: &[u8] = b"unknown";

impl<'a> Ctx<'a> {
    /// `None` when something doesn't fit V2's compact encoding (V1 fallback).
    pub fn build(window: Window, products: &'a [u8], campaigns: &'a [u8]) -> Option<Self> {
        let span = window.end.checked_sub(window.start)?.checked_add(LOOKBACK_MS)?;
        if window.end <= window.start || span > u32::MAX as i64 || window.start % 1_000 != 0 {
            return None;
        }

        // products.csv: later lines win, like V1's map.
        let mut product_rows: Vec<(u128, &[u8])> = Vec::new();
        let mut names: Vec<&[u8]> = vec![UNKNOWN];
        for line in csv::data_lines(products) {
            if let Some([Some(key), Some(value)]) = csv::split::<2>(line) {
                product_rows.push((id::pack(key)?, value));
                names.push(value);
            }
        }
        names.sort_unstable();
        names.dedup();
        if names.len() > u16::MAX as usize {
            return None;
        }
        let index = |name: &[u8]| names.binary_search(&name).unwrap() as u16;
        let mut product_map = HashMap::with_capacity(product_rows.len());
        for (key, value) in product_rows {
            product_map.insert(key, index(value));
        }
        let unknown_cat = index(UNKNOWN);

        let mut campaign_map: HashMap<u128, u16> = HashMap::new();
        let mut campaign_ids = Vec::new();
        let mut channels = Vec::new();
        for line in csv::data_lines(campaigns) {
            if let Some([Some(key), Some(value)]) = csv::split::<2>(line) {
                let packed = id::pack(key)?;
                match campaign_map.get(&packed) {
                    Some(&i) => channels[i as usize] = value,
                    None => {
                        if campaign_ids.len() >= u16::MAX as usize {
                            return None;
                        }
                        campaign_map.insert(packed, campaign_ids.len() as u16);
                        campaign_ids.push(key);
                        channels.push(value);
                    }
                }
            }
        }

        let day0 = crate::ts::day_of(window.start);
        let n_days = (crate::ts::day_of(window.end - 1) - day0 + 1) as usize;
        Some(Ctx {
            window,
            click_base: window.start - LOOKBACK_MS,
            products: product_map,
            categories: names,
            unknown_cat,
            campaigns: campaign_map,
            campaign_ids,
            channels,
            day0,
            n_days,
        })
    }

    /// Index of the `organic` row in campaign arrays.
    pub fn organic(&self) -> usize {
        self.campaign_ids.len()
    }
}
