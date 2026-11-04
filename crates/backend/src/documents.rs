//! Finding and counting documents.

use std::time::Duration;

use futures_util::TryStreamExt as _;
use mongodb::bson::{Bson, Document};
use mongodb::options::{Collation, CountOptions, FindOptions, Hint};
use uuid::Uuid;

use crate::{Backend, BackendError};

/// Counting stops after this long; the UI then shows "many".
pub(crate) const COUNT_TIME_LIMIT: Duration = Duration::from_secs(5);

/// Server error code when `maxTimeMS` runs out.
pub(crate) const MAX_TIME_EXPIRED: i32 = 50;

/// Documents bigger than this, as text, are cut in the preview.
pub const PREVIEW_BYTES: usize = 1 << 20;

/// The collection a command works on.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Namespace {
    pub connection: Uuid,
    pub database: String,
    pub collection: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct FindQuery {
    pub filter: Document,
    pub projection: Option<Document>,
    pub sort: Option<Document>,
    pub collation: Option<Document>,
    /// An index name (string) or key pattern (document).
    pub hint: Option<Bson>,
    pub skip: u64,
    pub limit: i64,
    pub max_time: Option<Duration>,
}

impl Backend {
    pub(crate) async fn find(
        &self,
        namespace: &Namespace,
        query: FindQuery,
    ) -> Result<Vec<Document>, BackendError> {
        let client = self.session(namespace.connection)?.client;
        let mut options = FindOptions::default();
        options.projection = query.projection;
        options.sort = query.sort;
        options.collation = query.collation.map(collation).transpose()?;
        options.hint = query.hint.map(to_hint).transpose()?;
        options.skip = (query.skip > 0).then_some(query.skip);
        options.limit = (query.limit > 0).then_some(query.limit);
        options.max_time = query.max_time;
        Ok(client
            .database(&namespace.database)
            .collection::<Document>(&namespace.collection)
            .find(query.filter)
            .with_options(options)
            .await?
            .try_collect()
            .await?)
    }

    /// `None` when counting took too long.
    pub(crate) async fn count(
        &self,
        namespace: &Namespace,
        filter: Document,
        hint: Option<Bson>,
    ) -> Result<Option<u64>, BackendError> {
        let client = self.session(namespace.connection)?.client;
        let collection = client
            .database(&namespace.database)
            .collection::<Document>(&namespace.collection);
        let result = if filter.is_empty() && hint.is_none() {
            // Uses collection metadata, so it's instant even for huge collections.
            collection.estimated_document_count().await
        } else {
            let mut options = CountOptions::default();
            options.hint = hint.map(to_hint).transpose()?;
            options.max_time = Some(COUNT_TIME_LIMIT);
            collection
                .count_documents(filter)
                .with_options(options)
                .await
        };
        match result {
            Ok(count) => Ok(Some(count)),
            Err(error) if server_code(&error) == Some(MAX_TIME_EXPIRED) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
}

/// The server's error code, for errors from a command.
pub(crate) fn server_code(error: &mongodb::error::Error) -> Option<i32> {
    match error.kind.as_ref() {
        mongodb::error::ErrorKind::Command(command) => Some(command.code),
        _ => None,
    }
}

pub(crate) fn collation(document: Document) -> Result<Collation, BackendError> {
    mongodb::bson::deserialize_from_document(document)
        .map_err(|error| BackendError::new("Invalid collation.").with_detail(error))
}

pub(crate) fn to_hint(value: Bson) -> Result<Hint, BackendError> {
    match value {
        Bson::String(name) => Ok(Hint::Name(name)),
        Bson::Document(keys) => Ok(Hint::Keys(keys)),
        _ => Err(BackendError::new(
            "A hint is an index name in quotes or a key pattern like { field: 1 }.",
        )),
    }
}
