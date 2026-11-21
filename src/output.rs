//! Parquet outputs per `rules.md` "Outputs" and D10.
//!
//! `day` is `INT32` with the Parquet `DATE` logical type, counts are `INT64`,
//! strings are `BYTE_ARRAY` + `UTF8`, `top_category` is nullable, and data is
//! uncompressed.

use std::fs::File;
use std::io;
use std::path::Path;
use std::sync::Arc;

use parquet::basic::Compression;
use parquet::data_type::{ByteArray, ByteArrayType, DataType, Int32Type, Int64Type};
use parquet::file::properties::WriterProperties;
use parquet::file::writer::{SerializedColumnWriter, SerializedFileWriter};
use parquet::schema::parser::parse_message_type;

const USER_DAILY_SCHEMA: &str = "
message shoptimizer_user_daily {
  REQUIRED BINARY user_id (UTF8);
  REQUIRED INT32 day (DATE);
  REQUIRED INT64 sessions;
  REQUIRED INT64 events;
  REQUIRED INT64 revenue_cents;
  OPTIONAL BINARY top_category (UTF8);
}
";

const CAMPAIGN_DAILY_SCHEMA: &str = "
message shoptimizer_campaign_daily {
  REQUIRED BINARY campaign_id (UTF8);
  REQUIRED BINARY channel (UTF8);
  REQUIRED INT32 day (DATE);
  REQUIRED INT64 sessions;
  REQUIRED INT64 purchasing_sessions;
  REQUIRED INT64 revenue_cents;
  REQUIRED INT64 assisted_sessions;
}
";

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

fn parquet_err(e: parquet::errors::ParquetError) -> io::Error {
    io::Error::other(e)
}

fn write_required<T: DataType>(
    mut col: SerializedColumnWriter<'_>,
    values: &[T::T],
) -> io::Result<()> {
    col.typed::<T>()
        .write_batch(values, None, None)
        .map_err(parquet_err)?;
    col.close().map_err(parquet_err)
}

fn write_optional<T: DataType>(
    mut col: SerializedColumnWriter<'_>,
    values: &[T::T],
    def_levels: &[i16],
) -> io::Result<()> {
    col.typed::<T>()
        .write_batch(values, Some(def_levels), None)
        .map_err(parquet_err)?;
    col.close().map_err(parquet_err)
}

fn open_writer(dir: &Path, schema: &str, name: &str) -> io::Result<SerializedFileWriter<File>> {
    std::fs::create_dir_all(dir)?;
    let schema = Arc::new(parse_message_type(schema).map_err(parquet_err)?);
    let props = Arc::new(
        WriterProperties::builder()
            .set_compression(Compression::UNCOMPRESSED)
            .build(),
    );
    let file = File::create(dir.join(name))?;
    SerializedFileWriter::new(file, schema, props).map_err(parquet_err)
}

fn bytes<'a>(rows: impl Iterator<Item = &'a [u8]>) -> Vec<ByteArray> {
    rows.map(ByteArray::from).collect()
}

/// Writes `rows` as one `part-0.parquet` in `dir`, creating the directory.
///
/// Zero rows produce a valid Parquet file with the exact schema and no row
/// groups.
pub fn write_user_daily(dir: &Path, rows: &[UserDaily]) -> io::Result<()> {
    write_user_daily_named(dir, "part-0.parquet", rows)
}

/// Like [`write_user_daily`], into file `name` (V2 writes one part per worker).
pub fn write_user_daily_named(dir: &Path, name: &str, rows: &[UserDaily]) -> io::Result<()> {
    let mut writer = open_writer(dir, USER_DAILY_SCHEMA, name)?;
    if !rows.is_empty() {
        let mut row_group = writer.next_row_group().map_err(parquet_err)?;

        let col = row_group
            .next_column()
            .map_err(parquet_err)?
            .expect("user_id");
        write_required::<ByteArrayType>(col, &bytes(rows.iter().map(|r| r.user_id)))?;

        let col = row_group.next_column().map_err(parquet_err)?.expect("day");
        write_required::<Int32Type>(col, &rows.iter().map(|r| r.day).collect::<Vec<_>>())?;

        let col = row_group
            .next_column()
            .map_err(parquet_err)?
            .expect("sessions");
        write_required::<Int64Type>(col, &rows.iter().map(|r| r.sessions).collect::<Vec<_>>())?;

        let col = row_group
            .next_column()
            .map_err(parquet_err)?
            .expect("events");
        write_required::<Int64Type>(col, &rows.iter().map(|r| r.events).collect::<Vec<_>>())?;

        let col = row_group
            .next_column()
            .map_err(parquet_err)?
            .expect("revenue_cents");
        write_required::<Int64Type>(
            col,
            &rows.iter().map(|r| r.revenue_cents).collect::<Vec<_>>(),
        )?;

        let mut categories = Vec::with_capacity(rows.len());
        let mut def_levels = Vec::with_capacity(rows.len());
        for row in rows {
            match row.top_category {
                Some(category) => {
                    categories.push(ByteArray::from(category));
                    def_levels.push(1i16);
                }
                None => def_levels.push(0i16),
            }
        }
        let col = row_group
            .next_column()
            .map_err(parquet_err)?
            .expect("top_category");
        write_optional::<ByteArrayType>(col, &categories, &def_levels)?;

        row_group.close().map_err(parquet_err)?;
    }
    writer.close().map_err(parquet_err)?;
    Ok(())
}

/// Writes `rows` as one `part-0.parquet` in `dir`, creating the directory.
///
/// Zero rows produce a valid Parquet file with the exact schema and no row
/// groups.
pub fn write_campaign_daily(dir: &Path, rows: &[CampaignDaily]) -> io::Result<()> {
    let mut writer = open_writer(dir, CAMPAIGN_DAILY_SCHEMA, "part-0.parquet")?;
    if !rows.is_empty() {
        let mut row_group = writer.next_row_group().map_err(parquet_err)?;

        let col = row_group
            .next_column()
            .map_err(parquet_err)?
            .expect("campaign_id");
        write_required::<ByteArrayType>(col, &bytes(rows.iter().map(|r| r.campaign_id)))?;

        let col = row_group
            .next_column()
            .map_err(parquet_err)?
            .expect("channel");
        write_required::<ByteArrayType>(col, &bytes(rows.iter().map(|r| r.channel)))?;

        let col = row_group.next_column().map_err(parquet_err)?.expect("day");
        write_required::<Int32Type>(col, &rows.iter().map(|r| r.day).collect::<Vec<_>>())?;

        let col = row_group
            .next_column()
            .map_err(parquet_err)?
            .expect("sessions");
        write_required::<Int64Type>(col, &rows.iter().map(|r| r.sessions).collect::<Vec<_>>())?;

        let col = row_group
            .next_column()
            .map_err(parquet_err)?
            .expect("purchasing_sessions");
        write_required::<Int64Type>(
            col,
            &rows
                .iter()
                .map(|r| r.purchasing_sessions)
                .collect::<Vec<_>>(),
        )?;

        let col = row_group
            .next_column()
            .map_err(parquet_err)?
            .expect("revenue_cents");
        write_required::<Int64Type>(
            col,
            &rows.iter().map(|r| r.revenue_cents).collect::<Vec<_>>(),
        )?;

        let col = row_group
            .next_column()
            .map_err(parquet_err)?
            .expect("assisted_sessions");
        write_required::<Int64Type>(
            col,
            &rows.iter().map(|r| r.assisted_sessions).collect::<Vec<_>>(),
        )?;

        row_group.close().map_err(parquet_err)?;
    }
    writer.close().map_err(parquet_err)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use parquet::basic::{ConvertedType, Type as PhysicalType};
    use parquet::file::reader::{FileReader, SerializedFileReader};

    fn scratch_dir(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("shoptimizer-t01-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn empty_tables_have_the_exact_schema() {
        let dir = scratch_dir("schema");
        let user_dir = dir.join("user_daily");
        let campaign_dir = dir.join("campaign_daily");

        write_user_daily(&user_dir, &[]).unwrap();
        write_campaign_daily(&campaign_dir, &[]).unwrap();

        let user_file = File::open(user_dir.join("part-0.parquet")).unwrap();
        let user = SerializedFileReader::new(user_file).unwrap();
        assert_eq!(user.metadata().file_metadata().num_rows(), 0);
        let schema = user.metadata().file_metadata().schema_descr();
        let expected: [(&str, PhysicalType, ConvertedType); 6] = [
            ("user_id", PhysicalType::BYTE_ARRAY, ConvertedType::UTF8),
            ("day", PhysicalType::INT32, ConvertedType::DATE),
            ("sessions", PhysicalType::INT64, ConvertedType::NONE),
            ("events", PhysicalType::INT64, ConvertedType::NONE),
            ("revenue_cents", PhysicalType::INT64, ConvertedType::NONE),
            (
                "top_category",
                PhysicalType::BYTE_ARRAY,
                ConvertedType::UTF8,
            ),
        ];
        assert_eq!(schema.num_columns(), expected.len());
        for (idx, (name, physical, converted)) in expected.iter().enumerate() {
            let col = schema.column(idx);
            assert_eq!(col.name(), *name);
            assert_eq!(col.physical_type(), *physical);
            assert_eq!(col.converted_type(), *converted);
        }
        assert!(schema.column(5).self_type().is_optional());

        let campaign_file = File::open(campaign_dir.join("part-0.parquet")).unwrap();
        let campaign = SerializedFileReader::new(campaign_file).unwrap();
        assert_eq!(campaign.metadata().file_metadata().num_rows(), 0);
        let schema = campaign.metadata().file_metadata().schema_descr();
        let expected: [(&str, PhysicalType, ConvertedType); 7] = [
            ("campaign_id", PhysicalType::BYTE_ARRAY, ConvertedType::UTF8),
            ("channel", PhysicalType::BYTE_ARRAY, ConvertedType::UTF8),
            ("day", PhysicalType::INT32, ConvertedType::DATE),
            ("sessions", PhysicalType::INT64, ConvertedType::NONE),
            (
                "purchasing_sessions",
                PhysicalType::INT64,
                ConvertedType::NONE,
            ),
            ("revenue_cents", PhysicalType::INT64, ConvertedType::NONE),
            (
                "assisted_sessions",
                PhysicalType::INT64,
                ConvertedType::NONE,
            ),
        ];
        assert_eq!(schema.num_columns(), expected.len());
        for (idx, (name, physical, converted)) in expected.iter().enumerate() {
            let col = schema.column(idx);
            assert_eq!(col.name(), *name);
            assert_eq!(col.physical_type(), *physical);
            assert_eq!(col.converted_type(), *converted);
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rows_round_trip_with_nullable_category() {
        let dir = scratch_dir("rows");
        let user_dir = dir.join("user_daily");
        write_user_daily(
            &user_dir,
            &[
                UserDaily {
                    user_id: b"u1",
                    day: 20729,
                    sessions: 2,
                    events: 3,
                    revenue_cents: 500,
                    top_category: Some(b"shoes"),
                },
                UserDaily {
                    user_id: b"u2",
                    day: 20730,
                    sessions: 1,
                    events: 1,
                    revenue_cents: 0,
                    top_category: None,
                },
            ],
        )
        .unwrap();

        let file = File::open(user_dir.join("part-0.parquet")).unwrap();
        let reader = SerializedFileReader::new(file).unwrap();
        assert_eq!(reader.metadata().file_metadata().num_rows(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
