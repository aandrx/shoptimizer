#!/usr/bin/env python3
"""
Shopping Sessions: slow, literal reference implementation of rules.md.

Clarity over speed. This is the checker the fast pipeline is diffed against,
so every step follows rules.md in its own order and wording. Values are kept
as raw bytes so comparisons are byte-wise, as the rules require.

Usage: reference.py [DATA_DIR OUTPUT_DIR]   (or env DATA_DIR / OUTPUT_DIR)
"""
import bisect
import datetime
import glob
import json
import os
import sys
from collections import defaultdict

SECOND = 1_000
MINUTE = 60 * SECOND
HOUR = 60 * MINUTE
DAY = 24 * HOUR
SESSION_GAP = 30 * MINUTE
LATE = 24 * HOUR
CLICK_LOOKBACK = 7 * DAY
MAX_PRICE = 10_000_000

DIGITS = frozenset(b"0123456789")


def log(*args):
    print(*args, file=sys.stderr, flush=True)


# --------------------------------------------------------------------------
# CSV dialect
# --------------------------------------------------------------------------

def read_lines(path):
    """Every data line of a file: only \\n ends a line, the first line is the header."""
    with open(path, "rb") as f:
        data = f.read()
    lines = data.split(b"\n")
    if lines and lines[-1] == b"":
        lines.pop()  # the text after the final \n is not a line
    return lines[1:]


def parse_line(line, nfields):
    """The fields of one line (None for null), or None if the line is malformed."""
    if line == b"":
        return None  # an empty line is malformed
    raw = line.split(b",")
    if len(raw) != nfields:
        return None
    fields = []
    for value in raw:
        if b'"' in value:
            # a quote may only wrap a whole field
            if len(value) < 2 or value[0:1] != b'"' or value[-1:] != b'"' or b'"' in value[1:-1]:
                return None
            value = value[1:-1]
        fields.append(value if value != b"" else None)
    return fields


def read_table(folder, nfields):
    for path in sorted(glob.glob(os.path.join(folder, "*.csv"))):
        for line in read_lines(path):
            fields = parse_line(line, nfields)
            if fields is not None:
                yield fields


# --------------------------------------------------------------------------
# Timestamps
# --------------------------------------------------------------------------

def days_from_civil(year, month, day):
    year -= month <= 2
    era = year // 400
    yoe = year - era * 400
    doy = (153 * ((month + 9) % 12) + 2) // 5 + day - 1
    doe = yoe * 365 + yoe // 4 - yoe // 100 + doy
    return era * 146_097 + doe - 719_468


def is_leap(year):
    return year % 4 == 0 and (year % 100 != 0 or year % 400 == 0)


def days_in_month(year, month):
    if month == 2:
        return 29 if is_leap(year) else 28
    return 30 if month in (4, 6, 9, 11) else 31


def all_digits(b):
    return len(b) > 0 and all(c in DIGITS for c in b)


def parse_civil(s):
    """`YYYY-MM-DDTHH:MM:SS` (19 bytes) -> epoch ms, or None."""
    if len(s) != 19:
        return None
    if s[4:5] != b"-" or s[7:8] != b"-" or s[10:11] != b"T" or s[13:14] != b":" or s[16:17] != b":":
        return None
    parts = (s[0:4], s[5:7], s[8:10], s[11:13], s[14:16], s[17:19])
    if not all(all_digits(p) for p in parts):
        return None
    year, month, day, hour, minute, second = (int(p) for p in parts)
    if not (1 <= month <= 12) or not (1 <= day <= days_in_month(year, month)):
        return None
    if hour > 23 or minute > 59 or second > 59:
        return None
    return (days_from_civil(year, month, day) * DAY + hour * HOUR + minute * MINUTE + second * SECOND)


def parse_ts(s):
    """F1, F2 or F3 -> epoch ms (int, possibly huge for F3), or None if bad."""
    if s is None:
        return None
    if all_digits(s):  # F3: digits only, leading zeros allowed, any length
        return int(s)
    if len(s) == 20 and s[19:20] == b"Z":  # F1
        return parse_civil(s[:19])
    if len(s) == 25 and s[19:20] in (b"+", b"-") and s[22:23] == b":":  # F2
        local = parse_civil(s[:19])
        oh, om = s[20:22], s[23:25]
        if local is None or not all_digits(oh) or not all_digits(om):
            return None
        oh, om = int(oh), int(om)
        if oh > 23 or om > 59:
            return None
        offset = (oh * HOUR + om * MINUTE) * (1 if s[19:20] == b"+" else -1)
        return local - offset  # subtract the offset to get UTC
    return None


# --------------------------------------------------------------------------
# Linked IDs
# --------------------------------------------------------------------------

def build_people(links_dir):
    parent = {}

    def find(x):
        root = x
        while parent[root] != root:
            root = parent[root]
        while parent[x] != root:
            parent[x], x = root, parent[x]
        return root

    for a, b in read_table(links_dir, 2):
        if a is None or b is None:
            continue
        parent.setdefault(a, a)
        parent.setdefault(b, b)
        ra, rb = find(a), find(b)
        if ra != rb:
            parent[ra] = rb

    smallest = {}
    for x in parent:
        r = find(x)
        if r not in smallest or x < smallest[r]:
            smallest[r] = x
    name = {x: smallest[find(x)] for x in parent}
    return lambda uid: name.get(uid, uid)


# --------------------------------------------------------------------------
# Pipeline
# --------------------------------------------------------------------------

def run(data, out):
    with open(os.path.join(data, "manifest.json")) as f:
        manifest = json.load(f)
    window_start = parse_ts(manifest["window_start"].encode())
    window_end = parse_ts(manifest["window_end"].encode())
    click_start = window_start - CLICK_LOOKBACK
    log("window", manifest["window_start"], manifest["window_end"])

    products = {}
    for fields in (parse_line(l, 2) for l in read_lines(os.path.join(data, "products.csv"))):
        if fields is not None:
            products[fields[0]] = fields[1]
    campaigns = {}
    for fields in (parse_line(l, 2) for l in read_lines(os.path.join(data, "campaigns.csv"))):
        if fields is not None:
            campaigns[fields[0]] = fields[1]

    person = build_people(os.path.join(data, "links"))

    # ---- Events: parse, dedup on event_id (smallest ingest_ts), validate, drop late
    kept = {}  # event_id -> (ingest_ms, fields)
    for fields in read_table(os.path.join(data, "events"), 7):
        event_id = fields[0]
        if event_id is None:
            continue
        ingest = parse_ts(fields[3])
        if ingest is None:
            ingest = float("inf")  # rules.md: ingest_ts is always valid F1
        prev = kept.get(event_id)
        if prev is None or ingest < prev[0]:
            kept[event_id] = (ingest, fields)

    by_person = defaultdict(list)  # person -> [(ts, is_purchase, price, category)]
    for ingest, (event_id, user_id, ts_raw, _ingest_raw, event_type, product_id, price_raw) in kept.values():
        if user_id is None or product_id is None:
            continue
        ts = parse_ts(ts_raw)
        if ts is None or not (window_start <= ts < window_end):
            continue
        if event_type not in (b"view", b"cart", b"purchase"):
            continue
        price = 0
        if event_type == b"purchase":
            if price_raw is None or not all_digits(price_raw):
                continue
            price = int(price_raw)
            if not (1 <= price <= MAX_PRICE):
                continue
        if ingest - ts > LATE:
            continue
        category = products.get(product_id, b"unknown")
        by_person[person(user_id)].append((ts, event_type == b"purchase", price, category))
    del kept

    # ---- Clicks: parse, dedup on click_id, validate
    seen_clicks = set()
    clicks_by_person = defaultdict(list)  # person -> [(ts, click_id, campaign_id)]
    for click_id, user_id, campaign_id, ts_raw in read_table(os.path.join(data, "clicks"), 4):
        if click_id is None or click_id in seen_clicks:
            continue
        seen_clicks.add(click_id)
        if user_id is None or campaign_id not in campaigns:
            continue
        ts = parse_ts(ts_raw)
        if ts is None or not (click_start <= ts < window_end):
            continue
        clicks_by_person[person(user_id)].append((ts, click_id, campaign_id))
    del seen_clicks
    for lst in clicks_by_person.values():
        lst.sort()

    # ---- Sessions, attribution, assists, aggregation
    user_daily = {}  # (person, day) -> [sessions, events, revenue, {category: cents}]
    campaign_daily = defaultdict(lambda: [0, 0, 0, 0])  # (campaign, day) -> [sess, purch, rev, assisted]

    for who, events in by_person.items():
        events.sort(key=lambda e: e[0])
        sessions = []
        prev_ts = None
        for event in events:
            if prev_ts is None or event[0] - prev_ts > SESSION_GAP:
                sessions.append([])
            sessions[-1].append(event)
            prev_ts = event[0]

        clicks = clicks_by_person.get(who, [])
        click_ts = [c[0] for c in clicks]
        for session in sessions:
            start = session[0][0]
            day = start // DAY
            revenue = sum(e[2] for e in session if e[1])
            purchased = any(e[1] for e in session)

            row = user_daily.setdefault((who, day), [0, 0, 0, defaultdict(int)])
            row[0] += 1
            row[1] += len(session)
            row[2] += revenue
            for e in session:
                if e[1]:
                    row[3][e[3]] += e[2]

            lo = bisect.bisect_left(click_ts, start - CLICK_LOOKBACK)
            hi = bisect.bisect_right(click_ts, start)
            window = clicks[lo:hi]
            if window:
                latest = max(c[0] for c in window)
                credit = min(c[1] for c in window if c[0] == latest)
                campaign = next(c[2] for c in window if c[0] == latest and c[1] == credit)
            else:
                campaign = b"organic"
            agg = campaign_daily[(campaign, day)]
            agg[0] += 1
            agg[1] += purchased
            agg[2] += revenue
            for assisted in {c[2] for c in window}:
                campaign_daily[(assisted, day)][3] += 1

    write_outputs(out, user_daily, campaign_daily, campaigns)
    log("user_daily rows", len(user_daily), "campaign_daily rows", len(campaign_daily))


def top_category(cents_by_category):
    if not cents_by_category:
        return None
    best = max(cents_by_category.values())
    return min(c for c, v in cents_by_category.items() if v == best)


def write_outputs(out, user_daily, campaign_daily, campaigns):
    import polars as pl  # only used to write Parquet

    epoch = datetime.date(1970, 1, 1)
    s = lambda b: None if b is None else b.decode("utf-8")
    ud = sorted(user_daily.items())
    user_df = pl.DataFrame(
        {
            "user_id": [s(k[0]) for k, _ in ud],
            "day": [epoch + datetime.timedelta(days=k[1]) for k, _ in ud],
            "sessions": [v[0] for _, v in ud],
            "events": [v[1] for _, v in ud],
            "revenue_cents": [v[2] for _, v in ud],
            "top_category": [s(top_category(v[3])) for _, v in ud],
        },
        schema={"user_id": pl.String, "day": pl.Date, "sessions": pl.Int64, "events": pl.Int64,
                "revenue_cents": pl.Int64, "top_category": pl.String},
    )
    cd = sorted(campaign_daily.items())
    camp_df = pl.DataFrame(
        {
            "campaign_id": [s(k[0]) for k, _ in cd],
            "channel": [s(b"organic" if k[0] == b"organic" else campaigns[k[0]]) for k, _ in cd],
            "day": [epoch + datetime.timedelta(days=k[1]) for k, _ in cd],
            "sessions": [v[0] for _, v in cd],
            "purchasing_sessions": [v[1] for _, v in cd],
            "revenue_cents": [v[2] for _, v in cd],
            "assisted_sessions": [v[3] for _, v in cd],
        },
        schema={"campaign_id": pl.String, "channel": pl.String, "day": pl.Date, "sessions": pl.Int64,
                "purchasing_sessions": pl.Int64, "revenue_cents": pl.Int64, "assisted_sessions": pl.Int64},
    )
    for name, df in (("user_daily", user_df), ("campaign_daily", camp_df)):
        os.makedirs(os.path.join(out, name), exist_ok=True)
        df.write_parquet(os.path.join(out, name, "part-0.parquet"))


if __name__ == "__main__":
    if len(sys.argv) >= 3:
        data_dir, out_dir = sys.argv[-2], sys.argv[-1]
    else:
        data_dir = os.environ.get("DATA_DIR", "/data")
        out_dir = os.environ.get("OUTPUT_DIR", "/output")
    run(data_dir, out_dir)
