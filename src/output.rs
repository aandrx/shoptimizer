//! Parquet writers for the two output tables (D10).

use std::fs::{self, File};
use std::io;
use std::path::Path;
use std::sync::Arc;

use parquet::basic::Compression;
use parquet::data_type::ByteArray;
use parquet::errors::ParquetError;
use parquet::file::properties::WriterProperties;
use parquet::file::writer::SerializedFileWriter;
use parquet::schema::parser::parse_message_type;

pub struct UserDaily<'a> {
    pub user_id: &'a [u8],
    pub day: i32,
    pub sessions: i64,
    pub events: i64,
    pub revenue_cents: i64,
    pub top_category: Option<&'a [u8]>,
}

pub struct CampaignDaily<'a> {
    pub campaign_id: &'a [u8],
    pub channel: &'a [u8],
    pub day: i32,
    pub sessions: i64,
    pub purchasing_sessions: i64,
    pub revenue_cents: i64,
    pub assisted_sessions: i64,
}

const USER_SCHEMA: &str = "message user_daily {
  REQUIRED BYTE_ARRAY user_id (STRING);
  REQUIRED INT32 day (DATE);
  REQUIRED INT64 sessions;
  REQUIRED INT64 events;
  REQUIRED INT64 revenue_cents;
  OPTIONAL BYTE_ARRAY top_category (STRING);
}";

const CAMPAIGN_SCHEMA: &str = "message campaign_daily {
  REQUIRED BYTE_ARRAY campaign_id (STRING);
  REQUIRED BYTE_ARRAY channel (STRING);
  REQUIRED INT32 day (DATE);
  REQUIRED INT64 sessions;
  REQUIRED INT64 purchasing_sessions;
  REQUIRED INT64 revenue_cents;
  REQUIRED INT64 assisted_sessions;
}";

const ROW_GROUP: usize = 1 << 20;

fn pq(e: ParquetError) -> io::Error {
    io::Error::new(io::ErrorKind::Other, e)
}

fn bytes<'a>(it: impl Iterator<Item = &'a [u8]>) -> Vec<ByteArray> {
    it.map(|b| ByteArray::from(b.to_vec())).collect()
}

macro_rules! col {
    ($rg:expr, $ty:ident, $vals:expr) => {{
        let mut c = $rg.next_column().map_err(pq)?.expect("column");
        c.typed::<parquet::data_type::$ty>().write_batch($vals, None, None).map_err(pq)?;
        c.close().map_err(pq)?;
    }};
    ($rg:expr, $ty:ident, $vals:expr, $def:expr) => {{
        let mut c = $rg.next_column().map_err(pq)?.expect("column");
        c.typed::<parquet::data_type::$ty>().write_batch($vals, Some($def), None).map_err(pq)?;
        c.close().map_err(pq)?;
    }};
}

fn open(dir: &Path, schema: &str) -> io::Result<SerializedFileWriter<File>> {
    fs::create_dir_all(dir)?;
    let schema = Arc::new(parse_message_type(schema).map_err(pq)?);
    let props = Arc::new(
        WriterProperties::builder()
            .set_compression(Compression::UNCOMPRESSED)
            .set_dictionary_enabled(false)
            .build(),
    );
    let file = File::create(dir.join("part-0.parquet"))?;
    SerializedFileWriter::new(file, schema, props).map_err(pq)
}

pub fn write_user_daily(dir: &Path, rows: &[UserDaily]) -> io::Result<()> {
    let mut w = open(dir, USER_SCHEMA)?;
    for chunk in rows.chunks(ROW_GROUP) {
        let mut rg = w.next_row_group().map_err(pq)?;
        col!(rg, ByteArrayType, &bytes(chunk.iter().map(|r| r.user_id)));
        col!(rg, Int32Type, &chunk.iter().map(|r| r.day).collect::<Vec<_>>());
        col!(rg, Int64Type, &chunk.iter().map(|r| r.sessions).collect::<Vec<_>>());
        col!(rg, Int64Type, &chunk.iter().map(|r| r.events).collect::<Vec<_>>());
        col!(rg, Int64Type, &chunk.iter().map(|r| r.revenue_cents).collect::<Vec<_>>());
        let cats = bytes(chunk.iter().filter_map(|r| r.top_category));
        let def: Vec<i16> = chunk.iter().map(|r| r.top_category.is_some() as i16).collect();
        col!(rg, ByteArrayType, &cats, &def);
        rg.close().map_err(pq)?;
    }
    w.close().map_err(pq)?;
    Ok(())
}

pub fn write_campaign_daily(dir: &Path, rows: &[CampaignDaily]) -> io::Result<()> {
    let mut w = open(dir, CAMPAIGN_SCHEMA)?;
    for chunk in rows.chunks(ROW_GROUP) {
        let mut rg = w.next_row_group().map_err(pq)?;
        col!(rg, ByteArrayType, &bytes(chunk.iter().map(|r| r.campaign_id)));
        col!(rg, ByteArrayType, &bytes(chunk.iter().map(|r| r.channel)));
        col!(rg, Int32Type, &chunk.iter().map(|r| r.day).collect::<Vec<_>>());
        col!(rg, Int64Type, &chunk.iter().map(|r| r.sessions).collect::<Vec<_>>());
        col!(rg, Int64Type, &chunk.iter().map(|r| r.purchasing_sessions).collect::<Vec<_>>());
        col!(rg, Int64Type, &chunk.iter().map(|r| r.revenue_cents).collect::<Vec<_>>());
        col!(rg, Int64Type, &chunk.iter().map(|r| r.assisted_sessions).collect::<Vec<_>>());
        rg.close().map_err(pq)?;
    }
    w.close().map_err(pq)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use parquet::file::reader::{FileReader, SerializedFileReader};
    use parquet::record::RowAccessor;

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("shopt-{}-{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    fn read(dir: &Path) -> (SerializedFileReader<File>, Vec<parquet::record::Row>) {
        let r = SerializedFileReader::new(File::open(dir.join("part-0.parquet")).unwrap()).unwrap();
        let rows = r.get_row_iter(None).unwrap().map(|x| x.unwrap()).collect();
        (r, rows)
    }

    #[test]
    fn user_daily_roundtrip() {
        let d = tmp("user");
        let rows = [
            UserDaily { user_id: b"u1", day: 19000, sessions: 2, events: 5, revenue_cents: 700, top_category: Some(b"shoes") },
            UserDaily { user_id: b"U2", day: -1, sessions: 1, events: 1, revenue_cents: 0, top_category: None },
        ];
        write_user_daily(&d, &rows).unwrap();
        let (r, got) = read(&d);
        let s = r.metadata().file_metadata().schema_descr();
        assert_eq!(s.column(1).physical_type(), parquet::basic::Type::INT32);
        assert_eq!(s.column(1).logical_type_ref(), Some(&parquet::basic::LogicalType::Date));
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].get_string(0).unwrap(), "u1");
        assert_eq!(got[0].get_column_iter().nth(1).unwrap().1, &parquet::record::Field::Date(19000));
        assert_eq!(got[0].get_long(4).unwrap(), 700);
        assert_eq!(got[0].get_string(5).unwrap(), "shoes");
        assert_eq!(got[1].get_column_iter().nth(1).unwrap().1, &parquet::record::Field::Date(-1));
        assert!(got[1].get_string(5).is_err()); // null
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn campaign_daily_roundtrip_and_empty() {
        let d = tmp("camp");
        let rows = [CampaignDaily { campaign_id: b"organic", channel: b"organic", day: 5, sessions: 3, purchasing_sessions: 1, revenue_cents: 99, assisted_sessions: 0 }];
        write_campaign_daily(&d, &rows).unwrap();
        let (_, got) = read(&d);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].get_string(1).unwrap(), "organic");
        assert_eq!(got[0].get_long(5).unwrap(), 99);
        write_campaign_daily(&d, &[]).unwrap();
        assert_eq!(read(&d).1.len(), 0);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn many_rows() {
        let d = tmp("many");
        let ids: Vec<String> = (0..2_500_000).map(|i| format!("u{i}")).collect();
        let rows: Vec<UserDaily> = ids
            .iter()
            .enumerate()
            .map(|(i, s)| UserDaily {
                user_id: s.as_bytes(),
                day: 1,
                sessions: i as i64,
                events: 1,
                revenue_cents: 0,
                top_category: if i % 2 == 0 { Some(b"c") } else { None },
            })
            .collect();
        write_user_daily(&d, &rows).unwrap();
        let r = SerializedFileReader::new(File::open(d.join("part-0.parquet")).unwrap()).unwrap();
        assert_eq!(r.metadata().file_metadata().num_rows(), 2_500_000);
        let _ = fs::remove_dir_all(&d);
    }
}
