//! KV, R2 and D1 wrappers. Every method returns a `Send` future, so they work inside axum handlers.
//!
//! Get one from [`Ctx`](crate::Ctx) with a binding constant from the generated `src/bindings.rs`:
//! `ctx.kv(bindings::CACHE)?`.

use axum::{
    body::Body,
    http::{
        Response,
        header::{CONTENT_LENGTH, CONTENT_TYPE, ETAG},
    },
};
use futures_util::StreamExt;
use serde::{Serialize, de::DeserializeOwned};
use worker::{
    KvStore, ResponseBody,
    d1::{D1Database, D1PreparedStatement, D1ResultMeta, serde_wasm_bindgen},
    js_sys::Array,
    send::{SendFuture, SendWrapper},
    wasm_bindgen::JsValue,
};

use crate::{Error, Result};

macro_rules! binding {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub struct $name(pub(crate) &'static str);

        impl $name {
            pub const fn new(name: &'static str) -> Self {
                Self(name)
            }
        }
    };
}

binding!(
    /// A `[kv.NAME]` binding from doko.toml.
    KvBinding
);
binding!(
    /// An `[r2.NAME]` binding from doko.toml.
    R2Binding
);
binding!(
    /// A `[d1.NAME]` binding from doko.toml.
    D1Binding
);
binding!(
    /// A `[queues.NAME]` binding from doko.toml.
    QueueBinding
);
binding!(
    /// A `[do.NAME]` Durable Object binding from doko.toml.
    DoBinding
);
binding!(
    /// An `[email.NAME]` sending binding from doko.toml.
    EmailBinding
);

pub(crate) fn missing(name: &str) -> impl FnOnce(worker::Error) -> Error + '_ {
    move |e| Error::internal(format!("binding `{name}` unavailable ({e}); is it in doko.toml, and did `doko config` run?"))
}

/// A KV namespace. Values are stored as JSON.
pub struct Kv(pub(crate) SendWrapper<KvStore>);

impl Kv {
    pub async fn get<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        Ok(SendFuture::new(self.0.get(key).json::<T>()).await.map_err(worker::Error::from)?)
    }

    pub async fn put<T: Serialize>(&self, key: &str, value: &T) -> Result<()> {
        let put = self.0.put(key, value).map_err(worker::Error::from)?;
        Ok(SendFuture::new(put.execute()).await.map_err(worker::Error::from)?)
    }

    /// Like [`put`](Self::put), expiring after `ttl_secs`. KV's minimum TTL is 60 seconds.
    pub async fn put_ttl<T: Serialize>(&self, key: &str, value: &T, ttl_secs: u64) -> Result<()> {
        if ttl_secs < 60 {
            return Err(Error::internal(format!("KV expiration_ttl must be at least 60 seconds (got {ttl_secs})")));
        }
        let put = self.0.put(key, value).map_err(worker::Error::from)?.expiration_ttl(ttl_secs);
        Ok(SendFuture::new(put.execute()).await.map_err(worker::Error::from)?)
    }

    pub async fn delete(&self, key: &str) -> Result<()> {
        Ok(SendFuture::new(self.0.delete(key)).await.map_err(worker::Error::from)?)
    }

    /// The underlying store, for list, metadata and streams. Its futures are not `Send`.
    pub fn raw(&self) -> &KvStore {
        &self.0
    }
}

/// The part size of streamed uploads. R2 needs at least 5 MiB, and equal parts except the last.
const PART_BYTES: usize = 8 * 1024 * 1024;

/// An R2 bucket.
pub struct Bucket(pub(crate) SendWrapper<worker::Bucket>);

impl Bucket {
    /// Streams the object as a response with `content-type`, `content-length` and `etag`, or a 404.
    /// The runtime streams the bytes, so this costs no worker CPU.
    pub async fn serve(&self, key: &str) -> Result<Response<Body>> {
        SendFuture::new(async {
            let Some(obj) = self.0.get(key).execute().await? else {
                return Err(Error::not_found());
            };
            let mut res = Response::builder().header(ETAG, obj.http_etag()).header(CONTENT_LENGTH, obj.size());
            if let Some(ct) = obj.http_metadata().content_type {
                res = res.header(CONTENT_TYPE, ct);
            }
            let body = match obj.body().map(|b| b.response_body()).transpose()? {
                Some(ResponseBody::Stream(s)) => Body::new(worker::Body::new(s)),
                Some(ResponseBody::Body(bytes)) => Body::from(bytes),
                _ => Body::empty(),
            };
            Ok(res.body(body)?)
        })
        .await
    }

    pub async fn get_bytes(&self, key: &str) -> Result<Option<Vec<u8>>> {
        SendFuture::new(async {
            match self.0.get(key).execute().await? {
                Some(obj) => Ok(match obj.body() {
                    Some(body) => Some(body.bytes().await?),
                    None => Some(Vec::new()),
                }),
                None => Ok(None),
            }
        })
        .await
    }

    /// Stores bytes already in memory. For request bodies, use [`put_stream`](Self::put_stream).
    pub async fn put(&self, key: &str, bytes: Vec<u8>, content_type: &str) -> Result<()> {
        let meta = worker::HttpMetadata { content_type: Some(content_type.to_owned()), ..Default::default() };
        SendFuture::new(self.0.put(key, bytes).http_metadata(meta).execute()).await?;
        Ok(())
    }

    /// Stores a body as it arrives, holding at most 8 MiB of it in memory: take the handler's
    /// `body: Body` and pass it on. Larger bodies go up as an R2 multipart upload, a part at a
    /// time; if the body fails midway, nothing is stored.
    pub async fn put_stream(&self, key: &str, body: Body, content_type: &str) -> Result<()> {
        let meta = worker::HttpMetadata { content_type: Some(content_type.to_owned()), ..Default::default() };
        SendFuture::new(async {
            let mut chunks = body.into_data_stream();
            let mut buf = Vec::new();
            let mut upload = None;
            let streamed: Result<()> = async {
                while let Some(chunk) = chunks.next().await {
                    buf.extend_from_slice(&chunk?);
                    while buf.len() >= PART_BYTES {
                        let rest = buf.split_off(PART_BYTES);
                        let part = std::mem::replace(&mut buf, rest);
                        if upload.is_none() {
                            let started = self.0.create_multipart_upload(key).http_metadata(meta.clone()).execute().await?;
                            upload = Some((started, Vec::new()));
                        }
                        let (upload, parts) = upload.as_mut().expect("just started");
                        parts.push(upload.upload_part(parts.len() as u16 + 1, part).await?);
                    }
                }
                if let Some((upload, parts)) = &mut upload
                    && !buf.is_empty()
                {
                    parts.push(upload.upload_part(parts.len() as u16 + 1, std::mem::take(&mut buf)).await?);
                }
                Ok(())
            }
            .await;
            match (streamed, upload) {
                // It all fit in one part: a plain put.
                (Ok(()), None) => self.0.put(key, buf).http_metadata(meta).execute().await.map(|_| ())?,
                (Ok(()), Some((upload, parts))) => upload.complete(parts).await.map(|_| ())?,
                (Err(e), upload) => {
                    if let Some((upload, _)) = upload {
                        // Otherwise the parts sent so far would be stored, and billed, until they expire.
                        let _ = upload.abort().await;
                    }
                    return Err(e);
                }
            }
            Ok(())
        })
        .await
    }

    pub async fn delete(&self, key: &str) -> Result<()> {
        Ok(SendFuture::new(self.0.delete(key)).await?)
    }

    /// The underlying bucket, for list, ranges and resumable multipart uploads. Its futures are not `Send`.
    pub fn raw(&self) -> &worker::Bucket {
        &self.0
    }
}

/// A D1 database. `params` is a tuple bound to `?1`, `?2`, ...: `(id,)`, `(name, age)`, or `()`.
pub struct Db(pub(crate) SendWrapper<D1Database>);

/// A prepared statement for [`Db::batch`].
pub struct Statement(SendWrapper<D1PreparedStatement>);

impl Db {
    /// All rows, deserialized by column name.
    pub async fn all<T: DeserializeOwned>(&self, sql: &str, params: impl Serialize) -> Result<Vec<T>> {
        let stmt = self.prepare(sql, params)?;
        Ok(SendFuture::new(async move { stmt.all().await?.results::<T>() }).await?)
    }

    /// The first row, if any.
    pub async fn first<T: DeserializeOwned>(&self, sql: &str, params: impl Serialize) -> Result<Option<T>> {
        let stmt = self.prepare(sql, params)?;
        Ok(SendFuture::new(async move { stmt.first::<T>(None).await }).await?)
    }

    /// Runs a write. The result has `changes` and `last_row_id`.
    pub async fn run(&self, sql: &str, params: impl Serialize) -> Result<D1ResultMeta> {
        let stmt = self.prepare(sql, params)?;
        let meta = SendFuture::new(async move { stmt.run().await?.meta() }).await?;
        meta.ok_or_else(|| Error::internal("D1 returned no result metadata"))
    }

    pub fn statement(&self, sql: &str, params: impl Serialize) -> Result<Statement> {
        Ok(Statement(SendWrapper::new(self.prepare(sql, params)?)))
    }

    /// Runs the statements in one transaction: all of them commit, or none do.
    pub async fn batch(&self, statements: Vec<Statement>) -> Result<()> {
        if statements.is_empty() {
            return Ok(());
        }
        let stmts = statements.into_iter().map(|s| s.0.0).collect();
        let results = SendFuture::new(self.0.batch(stmts)).await?;
        for res in results {
            if let Some(err) = res.error() {
                return Err(Error::internal(err));
            }
            if !res.success() {
                return Err(Error::internal("D1 batch statement failed"));
            }
        }
        Ok(())
    }

    /// The underlying database, for sessions, `exec` and `dump`. Its futures are not `Send`.
    pub fn raw(&self) -> &D1Database {
        &self.0
    }

    fn prepare(&self, sql: &str, params: impl Serialize) -> Result<D1PreparedStatement> {
        // D1 rejects undefined, so missing values (None) bind as NULL.
        let ser = serde_wasm_bindgen::Serializer::new().serialize_missing_as_null(true);
        let value = params.serialize(&ser).map_err(|e| Error::internal(format!("binding D1 params: {e}")))?;
        let args: Vec<JsValue> = if value.is_undefined() || value.is_null() {
            Vec::new()
        } else if Array::is_array(&value) {
            Array::from(&value).to_vec()
        } else {
            return Err(Error::internal("D1 params must be a tuple, e.g. `(id,)` or `()`"));
        };
        Ok(self.0.prepare(sql).bind(&args)?)
    }
}
