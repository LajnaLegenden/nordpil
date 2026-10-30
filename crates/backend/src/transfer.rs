//! Exporting and importing documents as JSON, NDJSON and CSV. Both stream,
//! so a big collection never sits in memory.

use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use futures_util::TryStreamExt as _;
use mongodb::bson::{Bson, Document};
use mongodb::options::{FindOptions, InsertManyOptions};

use crate::{Backend, BackendError, FindQuery, Namespace};

const INSERT_BATCH: usize = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// One JSON array of documents.
    JsonArray,
    /// One document per line, as mongoexport writes.
    Ndjson,
    Csv,
}

impl Format {
    /// Guesses from the file extension; JSON files may hold an array or lines.
    pub fn from_path(path: &Path) -> Option<Self> {
        match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
            "json" => Some(Self::JsonArray),
            "ndjson" | "jsonl" => Some(Self::Ndjson),
            "csv" => Some(Self::Csv),
            _ => None,
        }
    }
}

/// Extended JSON flavor: relaxed is readable (`5`, `"2024-…"`), canonical
/// keeps every type exact (`{"$numberInt": "5"}`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonMode {
    Relaxed,
    Canonical,
}

/// Shared with the UI, which reads it every frame. `total` is 0 when unknown.
#[derive(Debug, Default)]
pub struct Progress {
    /// Documents written (export) or bytes read (import).
    pub done: AtomicU64,
    pub total: AtomicU64,
}

#[derive(Debug)]
pub struct ExportJob {
    pub namespace: Namespace,
    /// Filter, projection, sort, skip and limit; `max_time` is ignored.
    pub query: FindQuery,
    pub path: PathBuf,
    pub format: Format,
    pub json_mode: JsonMode,
    /// CSV columns as dotted paths.
    pub fields: Vec<String>,
    pub progress: Arc<Progress>,
}

#[derive(Debug)]
pub struct ImportJob {
    pub namespace: Namespace,
    pub path: PathBuf,
    pub format: Format,
    /// CSV only: keep every value as a string instead of guessing types.
    pub csv_as_text: bool,
    pub progress: Arc<Progress>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportSummary {
    pub inserted: u64,
    /// Documents the server rejected, e.g. duplicate `_id`s.
    pub failed: u64,
    pub first_error: Option<String>,
}

impl Backend {
    pub(crate) async fn export(&self, job: ExportJob) -> Result<u64, BackendError> {
        let client = self.session(job.namespace.connection)?.client;
        let collection = client
            .database(&job.namespace.database)
            .collection::<Document>(&job.namespace.collection);
        if let Ok(Some(count)) = self
            .count(
                &job.namespace,
                job.query.filter.clone(),
                job.query.hint.clone(),
            )
            .await
        {
            let after_skip = count.saturating_sub(job.query.skip);
            let total = match u64::try_from(job.query.limit) {
                Ok(limit) if limit > 0 => after_skip.min(limit),
                _ => after_skip,
            };
            job.progress.total.store(total, Ordering::Relaxed);
        }

        let mut options = FindOptions::default();
        options.projection = job.query.projection;
        options.sort = job.query.sort;
        options.skip = (job.query.skip > 0).then_some(job.query.skip);
        options.limit = (job.query.limit > 0).then_some(job.query.limit);
        let mut cursor = collection
            .find(job.query.filter)
            .with_options(options)
            .await?;

        let file = PartFile::create(&job.path)?;
        let mut writer = Writer::new(
            BufWriter::new(file.handle()?),
            job.format,
            job.json_mode,
            &job.fields,
        )
        .map_err(|e| io_error(&job.path, e))?;
        let mut written = 0;
        while let Some(document) = cursor.try_next().await? {
            writer.write(document).map_err(|e| io_error(&job.path, e))?;
            written += 1;
            job.progress.done.store(written, Ordering::Relaxed);
        }
        writer.finish().map_err(|e| io_error(&job.path, e))?;
        file.commit()?;
        Ok(written)
    }

    pub(crate) async fn import(&self, job: ImportJob) -> Result<ImportSummary, BackendError> {
        let client = self.writable_session(job.namespace.connection)?.client;
        let collection = client
            .database(&job.namespace.database)
            .collection::<Document>(&job.namespace.collection);
        let file = File::open(&job.path).map_err(|e| io_error(&job.path, e))?;
        let size = file.metadata().map(|m| m.len()).unwrap_or(0);
        job.progress.total.store(size, Ordering::Relaxed);
        let reader = BufReader::new(Counting {
            inner: file,
            progress: Arc::clone(&job.progress),
        });

        let mut documents: Box<dyn Iterator<Item = Result<Document, String>> + Send> =
            match job.format {
                Format::Csv => Box::new(csv_documents(reader, job.csv_as_text)),
                Format::JsonArray | Format::Ndjson => Box::new(json_documents(reader)),
            };
        let mut summary = ImportSummary {
            inserted: 0,
            failed: 0,
            first_error: None,
        };
        loop {
            let mut batch = Vec::with_capacity(INSERT_BATCH);
            for document in documents.by_ref().take(INSERT_BATCH) {
                batch.push(document.map_err(|message| {
                    BackendError::new(format!(
                        "Stopped after {} documents: {message}",
                        summary.inserted + summary.failed
                    ))
                })?);
            }
            if batch.is_empty() {
                return Ok(summary);
            }
            let attempted = batch.len() as u64;
            let mut options = InsertManyOptions::default();
            // Unordered: one bad document (e.g. a duplicate _id) doesn't stop the rest.
            options.ordered = Some(false);
            match collection.insert_many(batch).with_options(options).await {
                Ok(result) => summary.inserted += result.inserted_ids.len() as u64,
                Err(error) => match error.kind.as_ref() {
                    mongodb::error::ErrorKind::InsertMany(failure) => {
                        let failed = failure.write_errors.as_ref().map_or(0, |e| e.len() as u64);
                        summary.inserted += attempted - failed;
                        summary.failed += failed;
                        if summary.first_error.is_none() {
                            summary.first_error = failure
                                .write_errors
                                .as_ref()
                                .and_then(|errors| errors.first())
                                .map(|e| e.message.clone());
                        }
                    }
                    _ => return Err(error.into()),
                },
            }
        }
    }
}

fn io_error(path: &Path, error: impl std::fmt::Display) -> BackendError {
    BackendError::new(format!("Could not write {}.", path.display())).with_detail(error)
}

/// Writes to `<path>.part` and renames it into place on `commit`. If the
/// export fails or is cancelled, the partial file is removed.
struct PartFile {
    part: PathBuf,
    path: PathBuf,
    committed: bool,
}

impl PartFile {
    fn create(path: &Path) -> Result<Self, BackendError> {
        let mut part = path.as_os_str().to_owned();
        part.push(".part");
        let part = PathBuf::from(part);
        File::create(&part).map_err(|e| io_error(&part, e))?;
        Ok(Self {
            part,
            path: path.to_owned(),
            committed: false,
        })
    }

    fn handle(&self) -> Result<File, BackendError> {
        fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&self.part)
            .map_err(|e| io_error(&self.part, e))
    }

    fn commit(mut self) -> Result<(), BackendError> {
        fs::rename(&self.part, &self.path).map_err(|e| io_error(&self.path, e))?;
        self.committed = true;
        Ok(())
    }
}

impl Drop for PartFile {
    fn drop(&mut self) {
        if !self.committed {
            let _ = fs::remove_file(&self.part);
        }
    }
}

enum Writer<'a, W: Write> {
    Json {
        out: W,
        array: bool,
        mode: JsonMode,
        first: bool,
    },
    Csv {
        out: Box<csv::Writer<W>>,
        fields: &'a [String],
    },
}

impl<'a, W: Write> Writer<'a, W> {
    fn new(mut out: W, format: Format, mode: JsonMode, fields: &'a [String]) -> io::Result<Self> {
        Ok(match format {
            Format::Csv => {
                let mut out = Box::new(csv::Writer::from_writer(out));
                out.write_record(fields)?;
                Self::Csv { out, fields }
            }
            Format::JsonArray => {
                out.write_all(b"[")?;
                Self::Json {
                    out,
                    array: true,
                    mode,
                    first: true,
                }
            }
            Format::Ndjson => Self::Json {
                out,
                array: false,
                mode,
                first: true,
            },
        })
    }

    fn write(&mut self, document: Document) -> io::Result<()> {
        match self {
            Self::Json {
                out,
                array,
                mode,
                first,
            } => {
                let value = match mode {
                    JsonMode::Relaxed => Bson::Document(document).into_relaxed_extjson(),
                    JsonMode::Canonical => Bson::Document(document).into_canonical_extjson(),
                };
                if *array {
                    out.write_all(if *first { b"\n" } else { b",\n" })?;
                }
                serde_json::to_writer(&mut *out, &value)?;
                if !*array {
                    out.write_all(b"\n")?;
                }
                *first = false;
                Ok(())
            }
            Self::Csv { out, fields } => {
                let record = fields
                    .iter()
                    .map(|field| csv_cell(lookup(&document, field)));
                out.write_record(record.collect::<Vec<_>>())?;
                Ok(())
            }
        }
    }

    fn finish(self) -> io::Result<()> {
        match self {
            Self::Json { mut out, array, .. } => {
                if array {
                    out.write_all(b"\n]\n")?;
                }
                out.flush()
            }
            Self::Csv { mut out, .. } => out.flush(),
        }
    }
}

/// Follows a dotted path through sub-documents.
fn lookup<'a>(document: &'a Document, path: &str) -> Option<&'a Bson> {
    let mut parts = path.split('.');
    let mut value = document.get(parts.next()?)?;
    for part in parts {
        value = match value {
            Bson::Document(child) => child.get(part)?,
            _ => return None,
        };
    }
    Some(value)
}

/// Plain text for simple values; relaxed Extended JSON for the rest.
fn csv_cell(value: Option<&Bson>) -> String {
    match value {
        None | Some(Bson::Null) => String::new(),
        Some(Bson::String(text)) => text.clone(),
        Some(Bson::Int32(n)) => n.to_string(),
        Some(Bson::Int64(n)) => n.to_string(),
        Some(Bson::Double(n)) => n.to_string(),
        Some(Bson::Boolean(b)) => b.to_string(),
        Some(Bson::ObjectId(id)) => id.to_hex(),
        Some(Bson::Decimal128(n)) => n.to_string(),
        Some(Bson::DateTime(date)) => date
            .try_to_rfc3339_string()
            .unwrap_or_else(|_| date.timestamp_millis().to_string()),
        Some(other) => other.clone().into_relaxed_extjson().to_string(),
    }
}

/// Counts bytes read into the shared progress.
struct Counting<R> {
    inner: R,
    progress: Arc<Progress>,
}

impl<R: Read> Read for Counting<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let read = self.inner.read(buf)?;
        self.progress.done.fetch_add(read as u64, Ordering::Relaxed);
        Ok(read)
    }
}

/// Documents from a JSON array, NDJSON, or concatenated JSON objects.
/// Values may use Extended JSON (`{"$oid": …}`) in either mode.
fn json_documents<R: BufRead + Send + 'static>(
    mut reader: R,
) -> impl Iterator<Item = Result<Document, String>> + Send {
    let starts_with_array = loop {
        match reader.fill_buf() {
            Ok([]) => break false,
            Ok(buf) => {
                let skip = buf.iter().take_while(|b| b.is_ascii_whitespace()).count();
                if skip < buf.len() {
                    break buf[skip] == b'[';
                }
                reader.consume(skip);
            }
            Err(_) => break false,
        }
    };
    let values: Box<dyn Iterator<Item = Result<serde_json::Value, String>> + Send> =
        if starts_with_array {
            Box::new(ArrayElements::new(reader).map(|element| {
                element.and_then(|bytes| serde_json::from_slice(&bytes).map_err(|e| e.to_string()))
            }))
        } else {
            Box::new(
                serde_json::Deserializer::from_reader(reader)
                    .into_iter::<serde_json::Value>()
                    .map(|value| value.map_err(|e| e.to_string())),
            )
        };
    values.enumerate().map(|(index, value)| {
        let number = index + 1;
        match value {
            Ok(serde_json::Value::Object(map)) => Document::try_from(map)
                .map_err(|e| format!("Document {number} is not valid Extended JSON: {e}")),
            Ok(_) => Err(format!("Item {number} is not a document.")),
            Err(error) => Err(format!("Document {number}: {error}")),
        }
    })
}

/// Splits a top-level JSON array into the raw bytes of its elements without
/// reading the whole array into memory.
struct ArrayElements<R> {
    reader: R,
    started: bool,
    done: bool,
}

impl<R: BufRead> ArrayElements<R> {
    fn new(reader: R) -> Self {
        Self {
            reader,
            started: false,
            done: false,
        }
    }

    fn next_byte(&mut self) -> io::Result<Option<u8>> {
        let buf = self.reader.fill_buf()?;
        let Some(&byte) = buf.first() else {
            return Ok(None);
        };
        self.reader.consume(1);
        Ok(Some(byte))
    }
}

impl<R: BufRead> Iterator for ArrayElements<R> {
    type Item = Result<Vec<u8>, String>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        let mut element = Vec::new();
        let mut depth = 0usize;
        let mut in_string = false;
        let mut escaped = false;
        loop {
            let byte = match self.next_byte() {
                Ok(Some(byte)) => byte,
                Ok(None) => {
                    self.done = true;
                    return Some(Err("The JSON array is missing its closing ].".to_owned()));
                }
                Err(error) => {
                    self.done = true;
                    return Some(Err(error.to_string()));
                }
            };
            if !self.started {
                if byte == b'[' {
                    self.started = true;
                }
                continue;
            }
            if in_string {
                element.push(byte);
                match byte {
                    _ if escaped => escaped = false,
                    b'\\' => escaped = true,
                    b'"' => in_string = false,
                    _ => {}
                }
                continue;
            }
            match byte {
                b'"' => {
                    in_string = true;
                    element.push(byte);
                }
                b'{' | b'[' => {
                    depth += 1;
                    element.push(byte);
                }
                b'}' | b']' if depth > 0 => {
                    depth -= 1;
                    element.push(byte);
                }
                b']' => {
                    self.done = true;
                    let trimmed = element.trim_ascii();
                    return (!trimmed.is_empty()).then(|| Ok(trimmed.to_vec()));
                }
                b',' if depth == 0 => {
                    let trimmed = element.trim_ascii();
                    if !trimmed.is_empty() {
                        return Some(Ok(trimmed.to_vec()));
                    }
                }
                _ => element.push(byte),
            }
        }
    }
}

/// Documents from CSV with a header row. Dotted headers (`address.city`)
/// make sub-documents. Empty cells are left out.
fn csv_documents<R: Read + Send + 'static>(
    reader: R,
    as_text: bool,
) -> impl Iterator<Item = Result<Document, String>> + Send {
    let mut reader = csv::Reader::from_reader(reader);
    let headers = reader.headers().cloned();
    let mut records = reader.into_records().enumerate();
    let mut failed = false;
    std::iter::from_fn(move || {
        if failed {
            return None;
        }
        let headers = match &headers {
            Ok(headers) => headers,
            Err(error) => {
                failed = true;
                return Some(Err(format!("Could not read the CSV header: {error}")));
            }
        };
        let (index, record) = records.next()?;
        let record = match record {
            Ok(record) => record,
            Err(error) => {
                failed = true;
                return Some(Err(format!("CSV row {}: {error}", index + 2)));
            }
        };
        let mut document = Document::new();
        for (header, cell) in headers.iter().zip(record.iter()) {
            if let Some(value) = infer(header, cell, as_text) {
                set_path(&mut document, header, value);
            }
        }
        Some(Ok(document))
    })
}

/// Guesses a CSV cell's type. Numbers with leading zeros (zip codes) stay
/// text, and a 24-hex-digit `_id` becomes an ObjectId.
fn infer(header: &str, cell: &str, as_text: bool) -> Option<Bson> {
    if cell.is_empty() {
        return None;
    }
    if as_text {
        return Some(Bson::String(cell.to_owned()));
    }
    if header == "_id"
        && let Ok(id) = mongodb::bson::oid::ObjectId::parse_str(cell)
    {
        return Some(Bson::ObjectId(id));
    }
    match cell {
        "true" => return Some(Bson::Boolean(true)),
        "false" => return Some(Bson::Boolean(false)),
        _ => {}
    }
    let digits = cell.strip_prefix('-').unwrap_or(cell);
    let leading_zero = digits.len() > 1 && digits.starts_with('0') && !digits.starts_with("0.");
    if !leading_zero {
        if let Ok(n) = cell.parse::<i64>() {
            return Some(i32::try_from(n).map_or(Bson::Int64(n), Bson::Int32));
        }
        if digits.starts_with(|c: char| c.is_ascii_digit())
            && let Ok(n) = cell.parse::<f64>()
            && n.is_finite()
        {
            return Some(Bson::Double(n));
        }
    }
    Some(Bson::String(cell.to_owned()))
}

fn set_path(document: &mut Document, path: &str, value: Bson) {
    match path.split_once('.') {
        Some((head, rest)) => {
            if !matches!(document.get(head), Some(Bson::Document(_))) {
                document.insert(head, Document::new());
            }
            if let Some(Bson::Document(child)) = document.get_mut(head) {
                set_path(child, rest, value);
            }
        }
        None => {
            document.insert(path, value);
        }
    }
}

#[cfg(test)]
mod tests {
    use mongodb::bson::doc;

    use super::*;

    fn json(text: &str) -> Vec<Result<Document, String>> {
        json_documents(io::Cursor::new(text.as_bytes().to_vec())).collect()
    }

    #[test]
    fn reads_json_arrays_lines_and_extended_json() {
        let expected = vec![
            Ok(doc! { "a": "x,]}", "b": [1, { "c": 2 }] }),
            Ok(
                doc! { "_id": mongodb::bson::oid::ObjectId::parse_str("65a1b2c3d4e5f60718293a4b").unwrap() },
            ),
        ];
        let array = r#" [ {"a": "x,]}", "b": [1, {"c": 2}]},
            {"_id": {"$oid": "65a1b2c3d4e5f60718293a4b"}} ] "#;
        let lines = "{\"a\": \"x,]}\", \"b\": [1, {\"c\": 2}]}\n{\"_id\": {\"$oid\": \"65a1b2c3d4e5f60718293a4b\"}}\n";
        assert_eq!(json(array), expected);
        assert_eq!(json(lines), expected);
        assert_eq!(json("[]"), vec![]);
    }

    #[test]
    fn reports_bad_json_items() {
        assert_eq!(
            json("[1]"),
            vec![Err("Item 1 is not a document.".to_owned())]
        );
        assert!(json("[{\"a\": 1}").last().unwrap().is_err());
    }

    #[test]
    fn reads_csv_with_types_and_nested_headers() {
        let text = "_id,name,zip,n,ratio,ok,address.city\n\
                    65a1b2c3d4e5f60718293a4b,Ada,0123,42,1.5,true,Oslo\n\
                    x,,,,,,\n";
        let documents: Vec<_> = csv_documents(io::Cursor::new(text.as_bytes().to_vec()), false)
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            documents,
            [
                doc! {
                    "_id": mongodb::bson::oid::ObjectId::parse_str("65a1b2c3d4e5f60718293a4b").unwrap(),
                    "name": "Ada", "zip": "0123", "n": 42, "ratio": 1.5, "ok": true,
                    "address": { "city": "Oslo" },
                },
                doc! { "_id": "x" },
            ]
        );
    }

    #[test]
    fn writes_csv_cells() {
        let document = doc! { "a": { "b": 5 }, "s": "x", "list": [1, 2] };
        assert_eq!(csv_cell(lookup(&document, "a.b")), "5");
        assert_eq!(csv_cell(lookup(&document, "missing")), "");
        assert_eq!(csv_cell(lookup(&document, "list")), "[1,2]");
    }
}
