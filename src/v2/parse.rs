//! Row parsers: one CSV line into a compact record (O1, O4, O5, O7).

use crate::csv;
use crate::id;
use crate::ts;
use crate::v1::{kind_of, parse_price};

use super::types::{Ck, Ctx, EvRaw, Parsed, LATE_MS, PURCHASE, VALID};

/// An event copy. `Skip` = malformed line or empty `event_id`. Invalid copies
/// are still `Rec` (without `VALID`), because dedup comes before validation.
#[inline]
pub fn event(line: &[u8], ctx: &Ctx) -> Parsed<EvRaw> {
    let Some([event_id, user, ts_raw, ingest_raw, kind, product, price_raw]) =
        csv::split::<7>(line)
    else {
        return Parsed::Skip;
    };
    let Some(event_id) = event_id else {
        return Parsed::Skip;
    };
    let Some(id) = id::pack(event_id) else {
        return Parsed::Overflow;
    };
    // ingest_ts is always valid F1; anything else can't be encoded exactly.
    let Some(ingest) = ingest_raw.and_then(ts::parse_ts) else {
        return Parsed::Overflow;
    };
    let Some(rel) = ingest.checked_sub(ctx.window.start) else {
        return Parsed::Overflow;
    };
    if rel % 1_000 != 0 {
        return Parsed::Overflow;
    }
    let Ok(ingest_s) = i32::try_from(rel / 1_000) else {
        return Parsed::Overflow;
    };
    let mut rec = EvRaw {
        id,
        user: 0,
        ingest_s,
        ts: 0,
        price: 0,
        cat: 0,
        flags: 0,
    };

    // rules.md Events step 3-4, all per copy.
    let Some(user) = user else {
        return Parsed::Rec(rec);
    };
    let Some(t) = ts_raw.and_then(ts::parse_ts) else {
        return Parsed::Rec(rec);
    };
    if t < ctx.window.start || t >= ctx.window.end {
        return Parsed::Rec(rec);
    }
    let kind = kind_of(kind);
    if kind == 0 {
        return Parsed::Rec(rec);
    }
    let Some(product) = product else {
        return Parsed::Rec(rec);
    };
    let purchase = kind == 3;
    let price = if purchase {
        let price = price_raw.map(parse_price).unwrap_or(0);
        if price == 0 {
            return Parsed::Rec(rec);
        }
        price
    } else {
        0
    };
    if ingest.saturating_sub(t) > LATE_MS {
        return Parsed::Rec(rec);
    }
    let Some(user) = id::pack(user) else {
        return Parsed::Overflow;
    };
    // A product too long to pack can't be in products.csv (Ctx::build checks).
    let cat = id::pack(product)
        .and_then(|p| ctx.products.get(&p).copied())
        .unwrap_or(ctx.unknown_cat);
    rec.user = user;
    rec.ts = (t - ctx.window.start) as u32;
    rec.price = price as u32;
    rec.cat = cat;
    rec.flags = VALID | if purchase { PURCHASE } else { 0 };
    Parsed::Rec(rec)
}

/// A click and its raw user. `Skip` = malformed or invalid (no click dedup, D7).
#[inline]
pub fn click(line: &[u8], ctx: &Ctx) -> Parsed<(u128, Ck)> {
    let Some([click_id, user, campaign, ts_raw]) = csv::split::<4>(line) else {
        return Parsed::Skip;
    };
    let (Some(click_id), Some(user), Some(campaign)) = (click_id, user, campaign) else {
        return Parsed::Skip;
    };
    let Some(campaign) = id::pack(campaign).and_then(|c| ctx.campaigns.get(&c).copied()) else {
        return Parsed::Skip;
    };
    let Some(t) = ts_raw.and_then(ts::parse_ts) else {
        return Parsed::Skip;
    };
    if t < ctx.click_base || t >= ctx.window.end {
        return Parsed::Skip;
    }
    let (Some(click_id), Some(user)) = (id::pack(click_id), id::pack(user)) else {
        return Parsed::Overflow;
    };
    Parsed::Rec((
        user,
        Ck {
            person: 0,
            click_id,
            ts: (t - ctx.click_base) as u32,
            campaign,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ts::Window;

    const PRODUCTS: &[u8] = b"product_id,category\np1,shoes\np2,books\n";
    const CAMPAIGNS: &[u8] = b"campaign_id,channel\nc_1,search\nc_2,social\n";

    fn ctx() -> Ctx<'static> {
        let window = Window::parse(
            br#"{"window_start": "2026-10-01T00:00:00Z", "window_end": "2026-10-15T00:00:00Z"}"#,
        )
        .unwrap();
        Ctx::build(window, PRODUCTS, CAMPAIGNS).unwrap()
    }

    fn ev(line: &str) -> Option<EvRaw> {
        match event(line.as_bytes(), &ctx()) {
            Parsed::Rec(r) => Some(r),
            Parsed::Skip => None,
            Parsed::Overflow => panic!("overflow"),
        }
    }

    fn valid(line: &str) -> bool {
        ev(line).is_some_and(|r| r.flags & VALID != 0)
    }

    #[test]
    fn event_validity() {
        assert!(valid("e1,u1,2026-10-03T10:00:00Z,2026-10-03T10:01:00Z,view,p1,"));
        // E5: an invalid copy still takes part in dedup.
        let bad = ev("e2,u1,2026-10-03 10:00:00,2026-10-03T10:01:00Z,view,p1,").unwrap();
        assert_eq!(bad.flags & VALID, 0);
        // E6: exactly 24 h kept, +1 s dropped.
        assert!(valid("e1,u1,2026-10-03T10:00:00Z,2026-10-04T10:00:00Z,view,p1,"));
        assert!(!valid("e1,u1,2026-10-03T10:00:00Z,2026-10-04T10:00:01Z,view,p1,"));
        // ingest before ts stays.
        assert!(valid("e1,u1,2026-10-03T10:00:00Z,2026-10-03T09:00:00Z,view,p1,"));
        // E10: price only matters for purchases.
        assert!(!valid("e1,u1,2026-10-03T10:00:00Z,2026-10-03T10:01:00Z,purchase,p1,19.99"));
        assert!(valid("e1,u1,2026-10-03T10:00:00Z,2026-10-03T10:01:00Z,view,p1,-5"));
        assert!(valid("e1,u1,2026-10-03T10:00:00Z,2026-10-03T10:01:00Z,purchase,p1,0250"));
        // E12: window edges.
        assert!(!valid("e1,u1,2026-09-30T23:59:59Z,2026-10-01T00:00:00Z,view,p1,"));
        assert!(!valid("e1,u1,2026-10-15T00:00:00Z,2026-10-15T00:00:00Z,view,p1,"));
        assert!(valid("e1,u1,2026-10-14T23:59:59Z,2026-10-15T00:00:00Z,view,p1,"));
        // E13: malformed lines and strict types.
        assert!(ev(",u1,2026-10-03T10:00:00Z,2026-10-03T10:01:00Z,view,p1,").is_none());
        assert!(ev("e1,u1,2026-10-03T10:00:00Z,2026-10-03T10:01:00Z,view,p1").is_none());
        assert!(!valid("e1,u1,2026-10-03T10:00:00Z,2026-10-03T10:01:00Z,Purchase,p1,100"));
        assert!(!valid("e1,u1,1791071890000.0,2026-10-03T10:01:00Z,view,p1,"));
        // E20: \r is data.
        assert!(!valid("e1,u1,2026-10-03T10:00:00Z,2026-10-03T10:01:00Z,purchase,p1,1000\r"));
        // Empty user / product.
        assert!(!valid("e1,,2026-10-03T10:00:00Z,2026-10-03T10:01:00Z,view,p1,"));
        assert!(!valid("e1,u1,2026-10-03T10:00:00Z,2026-10-03T10:01:00Z,view,,"));
    }

    #[test]
    fn event_encoding() {
        let c = ctx();
        let r = ev("\"e1\",u1,2026-10-03T10:00:00Z,2026-10-03T10:01:00Z,purchase,p9,500").unwrap();
        assert_eq!(r.id, id::pack(b"e1").unwrap());
        assert_eq!(r.user, id::pack(b"u1").unwrap());
        assert_eq!(r.ts, 2 * 86_400_000 + 10 * 3_600_000);
        assert_eq!(r.ingest_s, 2 * 86_400 + 10 * 3_600 + 60);
        assert_eq!(r.price, 500);
        assert_eq!(r.flags, VALID | PURCHASE);
        assert_eq!(c.categories[r.cat as usize], b"unknown");
        let r = ev("e1,u1,2026-10-03T10:00:00Z,2026-10-03T10:01:00Z,view,p2,").unwrap();
        assert_eq!(c.categories[r.cat as usize], b"books");
        // Categories are sorted byte-wise.
        assert_eq!(c.categories, vec![&b"books"[..], b"shoes", b"unknown"]);
    }

    fn ck(line: &str) -> Option<Ck> {
        match click(line.as_bytes(), &ctx()) {
            Parsed::Rec((_, c)) => Some(c),
            Parsed::Skip => None,
            Parsed::Overflow => panic!("overflow"),
        }
    }

    #[test]
    fn click_validity() {
        assert!(ck("k1,u1,c_1,2026-10-03T09:00:00Z").is_some());
        assert!(ck("k1,u1,c_999,2026-10-03T09:00:00Z").is_none()); // A6
        assert!(ck("k1,u1, c_1,2026-10-03T09:00:00Z").is_none()); // E19
        assert!(ck("k1,u1,c_1,2026-09-24T00:00:00Z").is_some()); // A10: window start - 7 d
        assert!(ck("k1,u1,c_1,1790208000000").is_some());
        assert!(ck("k1,u1,c_1,1790207999999").is_none());
        assert!(ck("k1,u1,c_1,2026-10-15T00:00:00Z").is_none());
        assert!(ck("k1,u1,c_1,2026-10-03T09:00:00Z\r").is_none()); // E20
        assert!(ck(",u1,c_1,2026-10-03T09:00:00Z").is_none());
        assert!(ck("k1,,c_1,2026-10-03T09:00:00Z").is_none());
        assert_eq!(ck("k1,u1,c_2,2026-09-24T00:00:00Z").unwrap().ts, 0);
    }

    #[test]
    fn long_ids_overflow() {
        let c = ctx();
        assert!(matches!(
            event(b"e1234567890123456,u1,2026-10-03T10:00:00Z,2026-10-03T10:01:00Z,view,p1,", &c),
            Parsed::Overflow
        ));
        assert!(matches!(
            click(b"k1,u1234567890123456,c_1,2026-10-03T09:00:00Z", &c),
            Parsed::Overflow
        ));
    }
}
