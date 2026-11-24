//! Postgres through Hyperdrive, behind the `pg` feature.
//!
//! Declare `[hyperdrive.PG]` in doko.toml and query it the way you query D1:
//! `ctx.pg(bindings::PG).await?.all::<Note>("SELECT * FROM notes WHERE id = $1", (id,)).await?`.
//! Placeholders are Postgres's `$1`, `$2`, not D1's `?1`.
//!
//! Hyperdrive pools the connections, so opening one per request is cheap.

use std::error::Error as StdError;

use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Map, Value};
use tokio_postgres::{
    Client, NoTls, Row,
    types::{FromSql, IsNull, ToSql, Type, private::BytesMut, to_sql_checked},
};
use worker::{console_error, send::SendFuture};

use crate::{Ctx, Error, HyperdriveBinding, Result, storage};

type Boxed = Box<dyn StdError + Sync + Send>;

/// A Postgres error with the server's own message, which tokio-postgres hides behind "db error".
fn pg_error(e: tokio_postgres::Error) -> Error {
    match e.as_db_error() {
        Some(db) => Error::internal(format!("postgres: {} ({})", db.message(), db.code().code())),
        None => Error::internal(format!("postgres: {e}")),
    }
}

impl Ctx {
    /// A Postgres connection through a `[hyperdrive.NAME]` binding: `ctx.pg(bindings::PG).await?`.
    pub async fn pg(&self, binding: HyperdriveBinding) -> Result<Pg> {
        let hyperdrive = self.env().hyperdrive(binding.0).map_err(storage::missing(binding.0))?;
        SendFuture::new(async move {
            let config: tokio_postgres::Config = hyperdrive.connection_string().parse().map_err(pg_error)?;
            // Hyperdrive's socket is already the secured path to the database.
            let (client, connection) = config.connect_raw(hyperdrive.connect()?, NoTls).await.map_err(pg_error)?;
            // The connection does the I/O for the client's queries; it ends when the client is dropped.
            worker::wasm_bindgen_futures::spawn_local(async move {
                if let Err(e) = connection.await {
                    console_error!(
                        "{}",
                        serde_json::json!({ "level": "error", "component": "pg", "error": e.to_string() })
                    );
                }
            });
            Ok(Pg(client))
        })
        .await
    }
}

/// A Postgres connection, from [`Ctx::pg`]. `params` is a tuple bound to `$1`, `$2`, ...:
/// `(id,)`, `(name, age)`, or `()`.
///
/// Rows are read by column name into your type. Text, numbers (not `numeric`), booleans, JSON,
/// UUIDs, dates and timestamps are understood; cast anything else in SQL: `price::text`.
pub struct Pg(Client);

/// A statement for [`Pg::batch`].
pub struct PgStatement {
    sql: String,
    params: Vec<Param>,
}

impl Pg {
    /// All rows, deserialized by column name.
    pub async fn all<T: DeserializeOwned>(&self, sql: &str, params: impl Serialize) -> Result<Vec<T>> {
        let params = bind(params)?;
        let rows = SendFuture::new(self.0.query(sql, &refs(&params))).await.map_err(pg_error)?;
        rows.iter().map(read_row).collect()
    }

    /// The first row, if any.
    pub async fn first<T: DeserializeOwned>(&self, sql: &str, params: impl Serialize) -> Result<Option<T>> {
        let params = bind(params)?;
        let rows = SendFuture::new(self.0.query(sql, &refs(&params))).await.map_err(pg_error)?;
        rows.first().map(read_row).transpose()
    }

    /// Runs a write and returns how many rows it changed. To get the new row back, use
    /// `INSERT .. RETURNING *` with [`first`](Self::first).
    pub async fn run(&self, sql: &str, params: impl Serialize) -> Result<u64> {
        let params = bind(params)?;
        SendFuture::new(self.0.execute(sql, &refs(&params))).await.map_err(pg_error)
    }

    pub fn statement(&self, sql: &str, params: impl Serialize) -> Result<PgStatement> {
        Ok(PgStatement { sql: sql.to_owned(), params: bind(params)? })
    }

    /// Runs the statements in one transaction: all of them commit, or none do.
    pub async fn batch(&self, statements: Vec<PgStatement>) -> Result<()> {
        SendFuture::new(async {
            self.0.batch_execute("BEGIN").await.map_err(pg_error)?;
            for statement in &statements {
                if let Err(e) = self.0.execute(&statement.sql, &refs(&statement.params)).await {
                    let _ = self.0.batch_execute("ROLLBACK").await;
                    return Err(pg_error(e));
                }
            }
            self.0.batch_execute("COMMIT").await.map_err(pg_error)
        })
        .await
    }

    /// The underlying client, for prepared statements, `COPY` and typed parameters.
    pub fn raw(&self) -> &Client {
        &self.0
    }
}

fn refs(params: &[Param]) -> Vec<&(dyn ToSql + Sync)> {
    params.iter().map(|p| p as &(dyn ToSql + Sync)).collect()
}

fn bind(params: impl Serialize) -> Result<Vec<Param>> {
    match serde_json::to_value(params)? {
        Value::Null => Ok(Vec::new()),
        Value::Array(values) => Ok(values.into_iter().map(Param).collect()),
        _ => Err(Error::internal("Postgres params must be a tuple, e.g. `(id,)` or `()`")),
    }
}

/// A JSON value bound as whatever type Postgres inferred for its placeholder.
#[derive(Debug)]
struct Param(Value);

impl ToSql for Param {
    fn to_sql(&self, ty: &Type, out: &mut BytesMut) -> Result<IsNull, Boxed> {
        let int = |n: &serde_json::Number| n.as_i64().ok_or_else(|| format!("{n} is not an integer"));
        match (&self.0, ty) {
            (Value::Null, _) => return Ok(IsNull::Yes),
            (v, &Type::JSON) => out.extend_from_slice(v.to_string().as_bytes()),
            (v, &Type::JSONB) => {
                // jsonb's binary form is a version byte, then the text.
                out.extend_from_slice(&[1]);
                out.extend_from_slice(v.to_string().as_bytes());
            }
            (Value::Bool(b), &Type::BOOL) => out.extend_from_slice(&[u8::from(*b)]),
            (Value::Number(n), &Type::INT2) => out.extend_from_slice(&i16::try_from(int(n)?)?.to_be_bytes()),
            (Value::Number(n), &Type::INT4) => out.extend_from_slice(&i32::try_from(int(n)?)?.to_be_bytes()),
            (Value::Number(n), &Type::INT8) => out.extend_from_slice(&int(n)?.to_be_bytes()),
            (Value::Number(n), &Type::FLOAT4) => {
                out.extend_from_slice(&(n.as_f64().unwrap_or(f64::NAN) as f32).to_be_bytes())
            }
            (Value::Number(n), &Type::FLOAT8) => out.extend_from_slice(&n.as_f64().unwrap_or(f64::NAN).to_be_bytes()),
            (Value::String(s), &Type::UUID) => out.extend_from_slice(&parse_uuid(s).ok_or("not a UUID")?),
            (Value::String(s), &(Type::TEXT | Type::VARCHAR | Type::BPCHAR | Type::NAME | Type::UNKNOWN)) => {
                out.extend_from_slice(s.as_bytes())
            }
            (Value::Number(n), &(Type::TEXT | Type::VARCHAR)) => out.extend_from_slice(n.to_string().as_bytes()),
            // shortcut: other types (timestamps, numeric, arrays) are not encoded; add them here when needed.
            (v, ty) => {
                return Err(format!("can't bind {v} as `{ty}`: write the placeholder as `$n::text::{ty}`").into());
            }
        }
        Ok(IsNull::No)
    }

    fn accepts(_: &Type) -> bool {
        true
    }

    to_sql_checked!();
}

/// A column value as JSON.
struct Cell(Value);

impl<'a> FromSql<'a> for Cell {
    fn from_sql(ty: &Type, raw: &'a [u8]) -> Result<Self, Boxed> {
        Ok(Cell(match *ty {
            Type::BOOL => Value::Bool(raw == [1]),
            Type::INT2 => i16::from_be_bytes(raw.try_into()?).into(),
            Type::INT4 => i32::from_be_bytes(raw.try_into()?).into(),
            Type::OID => u32::from_be_bytes(raw.try_into()?).into(),
            Type::INT8 => i64::from_be_bytes(raw.try_into()?).into(),
            Type::FLOAT4 => f64::from(f32::from_be_bytes(raw.try_into()?)).into(),
            Type::FLOAT8 => f64::from_be_bytes(raw.try_into()?).into(),
            Type::TEXT | Type::VARCHAR | Type::BPCHAR | Type::NAME | Type::UNKNOWN => std::str::from_utf8(raw)?.into(),
            Type::JSON => serde_json::from_slice(raw)?,
            Type::JSONB => serde_json::from_slice(raw.get(1..).unwrap_or_default())?,
            Type::UUID => format_uuid(raw.try_into()?).into(),
            Type::DATE => format_timestamp(i64::from(i32::from_be_bytes(raw.try_into()?)) * DAY_MICROS)[..10].into(),
            Type::TIMESTAMP => format_timestamp(i64::from_be_bytes(raw.try_into()?)).into(),
            Type::TIMESTAMPTZ => (format_timestamp(i64::from_be_bytes(raw.try_into()?)) + "Z").into(),
            _ => return Err(format!("column type `{ty}` isn't read yet: cast it in SQL, e.g. `col::text`").into()),
        }))
    }

    fn from_sql_null(_: &Type) -> Result<Self, Boxed> {
        Ok(Cell(Value::Null))
    }

    fn accepts(_: &Type) -> bool {
        true
    }
}

fn read_row<T: DeserializeOwned>(row: &Row) -> Result<T> {
    let mut object = Map::new();
    for (i, column) in row.columns().iter().enumerate() {
        let cell: Cell = row.try_get(i).map_err(|e| {
            let cause = e.source().map(ToString::to_string).unwrap_or_else(|| e.to_string());
            Error::internal(format!("postgres column `{}`: {cause}", column.name()))
        })?;
        object.insert(column.name().to_owned(), cell.0);
    }
    Ok(serde_json::from_value(Value::Object(object))?)
}

const DAY_MICROS: i64 = 86_400_000_000;

/// Postgres timestamps count microseconds from 2000-01-01. Formats one as `YYYY-MM-DDTHH:MM:SS.ffffff`.
fn format_timestamp(micros: i64) -> String {
    let (days, in_day) = (micros.div_euclid(DAY_MICROS), micros.rem_euclid(DAY_MICROS));
    // Days to a civil date (Howard Hinnant's algorithm), counted from 0000-03-01.
    let z = days + 730_425;
    let (era, day_of_era) = (z.div_euclid(146_097), z.rem_euclid(146_097));
    let year_of_era = (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 { month_index + 3 } else { month_index - 9 };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    let secs = in_day / 1_000_000;
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:06}",
        secs / 3600,
        secs / 60 % 60,
        secs % 60,
        in_day % 1_000_000
    )
}

fn format_uuid(bytes: [u8; 16]) -> String {
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &hex[..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..])
}

fn parse_uuid(text: &str) -> Option<[u8; 16]> {
    let hex: Vec<u8> = text.bytes().filter(|b| *b != b'-').collect();
    if hex.len() != 32 {
        return None;
    }
    let mut out = [0; 16];
    for (byte, pair) in out.iter_mut().zip(hex.chunks(2)) {
        *byte = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn encode(value: Value, ty: &Type) -> Result<Vec<u8>, String> {
        let mut out = BytesMut::new();
        Param(value).to_sql(ty, &mut out).map(|_| out.to_vec()).map_err(|e| e.to_string())
    }

    fn decode(ty: &Type, raw: &[u8]) -> Value {
        Cell::from_sql(ty, raw).unwrap().0
    }

    #[test]
    fn binds_params_as_the_placeholder_type() {
        assert_eq!(encode(json!(7), &Type::INT4).unwrap(), 7i32.to_be_bytes());
        assert_eq!(encode(json!(7), &Type::INT8).unwrap(), 7i64.to_be_bytes());
        assert_eq!(encode(json!(1.5), &Type::FLOAT8).unwrap(), 1.5f64.to_be_bytes());
        assert_eq!(encode(json!("hi"), &Type::TEXT).unwrap(), b"hi");
        assert_eq!(encode(json!(true), &Type::BOOL).unwrap(), [1]);
        assert_eq!(encode(json!({ "a": 1 }), &Type::JSONB).unwrap(), b"\x01{\"a\":1}");
        assert!(encode(json!(70_000), &Type::INT2).is_err(), "out of range, not truncated");
        assert!(encode(json!(1.5), &Type::INT4).is_err());
        assert!(encode(json!("2026-01-01"), &Type::TIMESTAMPTZ).unwrap_err().contains("$n::text::timestamptz"));
        let mut out = BytesMut::new();
        assert!(matches!(Param(Value::Null).to_sql(&Type::INT4, &mut out), Ok(IsNull::Yes)));

        assert_eq!(bind(()).unwrap().len(), 0);
        assert_eq!(bind((1, "a")).unwrap().len(), 2);
        assert!(bind(5).is_err(), "a bare value is not a tuple");
    }

    #[test]
    fn reads_columns_as_json() {
        assert_eq!(decode(&Type::INT4, &42i32.to_be_bytes()), json!(42));
        assert_eq!(decode(&Type::INT8, &(-1i64).to_be_bytes()), json!(-1));
        assert_eq!(decode(&Type::BOOL, &[1]), json!(true));
        assert_eq!(decode(&Type::TEXT, "héllo".as_bytes()), json!("héllo"));
        assert_eq!(decode(&Type::JSONB, b"\x01[1,2]"), json!([1, 2]));
        assert!(Cell::from_sql(&Type::NUMERIC, &[0; 8]).err().unwrap().to_string().contains("col::text"));
        assert!(Cell::from_sql(&Type::INT4, &[0; 3]).is_err(), "a short value is an error, not a panic");

        let uuid = "123e4567-e89b-12d3-a456-426614174000";
        assert_eq!(decode(&Type::UUID, &parse_uuid(uuid).unwrap()), json!(uuid));
        assert!(parse_uuid("not-a-uuid").is_none());
    }

    #[test]
    fn formats_timestamps() {
        assert_eq!(format_timestamp(0), "2000-01-01T00:00:00.000000");
        assert_eq!(format_timestamp(-1), "1999-12-31T23:59:59.999999");
        // 2026-11-24 10:30:15.25 UTC is 9824 days after 2000-01-01.
        let micros = 9_824 * DAY_MICROS + (10 * 3600 + 30 * 60 + 15) * 1_000_000 + 250_000;
        assert_eq!(format_timestamp(micros), "2026-11-24T10:30:15.250000");
        assert_eq!(format_timestamp(59 * DAY_MICROS), "2000-02-29T00:00:00.000000", "leap day");
        assert_eq!(decode(&Type::DATE, &9_824i32.to_be_bytes()), json!("2026-11-24"));
        assert_eq!(decode(&Type::TIMESTAMPTZ, &0i64.to_be_bytes()), json!("2000-01-01T00:00:00.000000Z"));
    }
}
