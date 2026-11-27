//! Workers Analytics Engine: write data points through an `[analytics.NAME]` binding with
//! [`Ctx::analytics`], and read them back with SQL through [`query`].
//!
//! ```ignore
//! ctx.analytics(bindings::USAGE)?.write(&DataPoint::new("0.7.1").blob("0.7.1").double(1.0))?;
//!
//! #[derive(Deserialize)]
//! struct Day { day: String, checks: f64 }
//! let token = ctx.secret("ANALYTICS_API_TOKEN")?;
//! let days: Vec<Day> = doko::analytics::query(&account_id, &token,
//!     "SELECT toDate(timestamp) AS day, SUM(_sample_interval) AS checks FROM usage GROUP BY day").await?;
//! ```

use serde::{Deserialize, de::DeserializeOwned};
use worker::{
    AnalyticsEngineDataPointBuilder, AnalyticsEngineDataset, Fetch, Headers, Method, Request, RequestInit,
    send::{SendFuture, SendWrapper},
};

use crate::{AnalyticsBinding, Ctx, Error, Result, storage};

/// Analytics Engine's own limits, checked before a write so a bad point is an error rather than a silent drop.
const MAX_INDEX_BYTES: usize = 96;
const MAX_BLOBS: usize = 20;
const MAX_DOUBLES: usize = 20;
const MAX_BLOB_BYTES: usize = 16 * 1024;

/// An Analytics Engine dataset, from [`Ctx::analytics`].
pub struct Analytics(SendWrapper<AnalyticsEngineDataset>);

impl Ctx {
    /// An Analytics Engine dataset: `ctx.analytics(bindings::USAGE)?.write(&point)?`.
    pub fn analytics(&self, binding: AnalyticsBinding) -> Result<Analytics> {
        Ok(Analytics(SendWrapper::new(self.env().analytics_engine(binding.0).map_err(storage::missing(binding.0))?)))
    }
}

/// One row for [`Analytics::write`]. The index is the sampling key: rows that share it are sampled
/// together, so use what you will filter or group by most (a customer, a version). In SQL the
/// index is `index1`, blobs are `blob1`..`blob20` and doubles `double1`..`double20`, in the order added.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DataPoint {
    pub index: String,
    pub blobs: Vec<String>,
    pub doubles: Vec<f64>,
}

impl DataPoint {
    pub fn new(index: impl Into<String>) -> Self {
        Self { index: index.into(), ..Self::default() }
    }

    pub fn blob(mut self, blob: impl Into<String>) -> Self {
        self.blobs.push(blob.into());
        self
    }

    pub fn double(mut self, double: impl Into<f64>) -> Self {
        self.doubles.push(double.into());
        self
    }

    fn check(&self) -> Result<()> {
        let fail = |why: String| Err(Error::internal(format!("analytics data point: {why}")));
        if self.index.len() > MAX_INDEX_BYTES {
            return fail(format!("the index is {} bytes, over {MAX_INDEX_BYTES}", self.index.len()));
        }
        if self.blobs.len() > MAX_BLOBS || self.doubles.len() > MAX_DOUBLES {
            return fail(format!("at most {MAX_BLOBS} blobs and {MAX_DOUBLES} doubles"));
        }
        let size: usize = self.blobs.iter().map(String::len).sum();
        if size > MAX_BLOB_BYTES {
            return fail(format!("the blobs are {size} bytes together, over {MAX_BLOB_BYTES}"));
        }
        Ok(())
    }
}

impl Analytics {
    /// Queues `point` for the dataset. It returns at once: the runtime sends points after the
    /// response, so a write never slows a request down. Under `doko dev` points are dropped.
    pub fn write(&self, point: &DataPoint) -> Result<()> {
        point.check()?;
        let mut builder = AnalyticsEngineDataPointBuilder::new();
        if !point.index.is_empty() {
            builder = builder.indexes([point.index.as_str()]);
        }
        let built =
            builder.blobs(point.blobs.iter().map(String::as_str)).doubles(point.doubles.iter().copied()).build();
        self.0.write_data_point(&built).map_err(|e| Error::internal(format!("analytics write: {e}")))
    }

    /// The `worker` binding, for what [`DataPoint`] doesn't cover (byte blobs).
    pub fn raw(&self) -> &AnalyticsEngineDataset {
        &self.0
    }
}

/// Runs `sql` against Analytics Engine's SQL API and deserializes each row. Needs the account id
/// and an API token with Account Analytics Read; a worker can't read datasets through its binding.
///
/// The SQL API has no parameters, so never put request input into `sql`. Integer columns, such as
/// a `SUM(_sample_interval)`, arrive as JSON strings: read them as `String`, or cast them in SQL
/// (`toFloat64(SUM(_sample_interval))`).
pub async fn query<T: DeserializeOwned>(account_id: &str, api_token: &str, sql: &str) -> Result<Vec<T>> {
    let url = format!("https://api.cloudflare.com/client/v4/accounts/{account_id}/analytics_engine/sql");
    let auth = format!("Bearer {api_token}");
    let body = sql.to_owned();
    let (status, text) = SendFuture::new(async move {
        let headers = Headers::new();
        headers.set("Authorization", &auth)?;
        let mut init = RequestInit::new();
        init.with_method(Method::Post).with_headers(headers).with_body(Some(body.into()));
        let mut res = Fetch::Request(Request::new_with_init(&url, &init)?).send().await?;
        Ok::<_, worker::Error>((res.status_code(), res.text().await?))
    })
    .await
    .map_err(|e| Error::internal(format!("analytics query: {e}")))?;
    if !(200..300).contains(&status) {
        return Err(Error::internal(format!("analytics query: {status} {}", text.trim())));
    }
    parse(&text)
}

fn parse<T: DeserializeOwned>(text: &str) -> Result<Vec<T>> {
    #[derive(Deserialize)]
    struct Answer<T> {
        data: Vec<T>,
    }
    serde_json::from_str::<Answer<T>>(text)
        .map(|a| a.data)
        .map_err(|e| Error::internal(format!("analytics query: unexpected answer ({e})")))
}

#[cfg(test)]
mod tests {
    use super::{DataPoint, parse};

    #[test]
    fn points_over_the_limits_are_refused() {
        assert!(DataPoint::new("v1").blob("a").double(1.0).check().is_ok());
        assert!(DataPoint::new("x".repeat(97)).check().is_err());
        assert!((0..21).fold(DataPoint::new(""), |p, _| p.blob("a")).check().is_err());
        assert!((0..21).fold(DataPoint::new(""), |p, _| p.double(1.0)).check().is_err());
        assert!(DataPoint::new("").blob("x".repeat(16 * 1024 + 1)).check().is_err());
    }

    #[test]
    fn rows_come_from_data() {
        #[derive(serde::Deserialize, Debug, PartialEq)]
        struct Row {
            day: String,
            checks: String,
        }
        let answer = r#"{"meta":[{"name":"day","type":"Date"},{"name":"checks","type":"UInt64"}],
            "data":[{"day":"2026-11-27","checks":"3"}],"rows":1,"rows_before_limit_at_least":1}"#;
        assert_eq!(parse::<Row>(answer).unwrap(), [Row { day: "2026-11-27".into(), checks: "3".into() }]);
        assert!(parse::<Row>("not json").is_err());
    }
}
