//! Times formatting and highlighting a document close to MongoDB's 16 MB limit.

use std::time::Instant;

use bson::{Bson, Document, doc};

fn main() {
    let items: Vec<Bson> = (0..120_000)
        .map(|n| Bson::Document(doc! { "sku": format!("SKU-{n:08}"), "qty": n, "price": f64::from(n) * 0.25, "tags": ["a", "b"] }))
        .collect();
    let mut document = Document::new();
    document.insert("_id", 1);
    document.insert("items", items);
    let bytes = document.to_vec().unwrap().len();

    let start = Instant::now();
    let text = nordpil_query::format_document(&document, nordpil_query::Style::Pretty(2));
    let formatted = start.elapsed();
    let start = Instant::now();
    let spans = nordpil_query::highlight(&text);
    let highlighted = start.elapsed();
    println!(
        "BSON {:.1} MB, text {:.1} MB, {} spans: format {formatted:?}, highlight {highlighted:?}",
        bytes as f64 / 1e6,
        text.len() as f64 / 1e6,
        spans.len()
    );
}
