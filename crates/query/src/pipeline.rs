//! Aggregation pipelines: the stages with their starting values and keys,
//! the expression operators, and moving between a pipeline's text and its
//! stages.

use bson::Document;

use crate::parser::{NodeKind, ParseError, parse};

/// What a value holds, which decides what completion offers in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Value {
    /// An aggregation expression: `'$field'`, a literal or `{ $op: … }`.
    Expression,
    /// A query filter, as in `$match`.
    Filter,
    /// Field paths as keys, as in `$sort`.
    Keys,
    /// Names the user picks, each with an expression, e.g. `$bucket`'s `output`.
    Outputs,
    /// An array of stages.
    Pipeline,
    /// A collection name in quotes.
    Collection,
    /// A field path in quotes, without `$`.
    Field,
    Bool,
    /// Anything else: constructors and plain values.
    Other,
}

/// A key of a stage's value document.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Key {
    pub name: &'static str,
    /// What follows the colon; `|` marks the cursor.
    pub template: &'static str,
    pub detail: &'static str,
    pub value: Value,
}

const fn key(
    name: &'static str,
    template: &'static str,
    detail: &'static str,
    value: Value,
) -> Key {
    Key {
        name,
        template,
        detail,
        value,
    }
}

/// An aggregation stage, e.g. `$group`.
#[derive(Debug, PartialEq, Eq)]
pub struct Stage {
    pub name: &'static str,
    /// The value a new stage starts with; `|` marks the cursor.
    pub template: &'static str,
    pub detail: &'static str,
    /// What the value is when it isn't a document of the keys below, e.g.
    /// the `'$tags'` of `{ $unwind: '$tags' }`.
    pub(crate) value: Value,
    pub(crate) keys: &'static [Key],
    /// What keys the stage doesn't know hold, e.g. `$group`'s outputs.
    pub(crate) other_keys: Value,
    /// Field paths are suggested as keys, as in `$project`.
    pub(crate) field_keys: bool,
}

impl Stage {
    /// Whether the stage writes to a collection. It must come last.
    pub fn writes(&self) -> bool {
        is_write(self.name)
    }

    pub(crate) fn key(&self, name: &str) -> Option<&'static Key> {
        self.keys.iter().find(|key| key.name == name)
    }

    /// What a key of its value document holds.
    pub(crate) fn key_value(&self, name: &str) -> Value {
        self.key(name).map_or(self.other_keys, |key| key.value)
    }
}

/// Whether `name` is `$out` or `$merge`, the stages that write.
pub fn is_write(name: &str) -> bool {
    matches!(name, "$out" | "$merge")
}

/// The stage called `name`, e.g. `$group`.
pub fn stage(name: &str) -> Option<&'static Stage> {
    STAGES.iter().find(|stage| stage.name == name)
}

const fn plain(
    name: &'static str,
    template: &'static str,
    detail: &'static str,
    value: Value,
) -> Stage {
    Stage {
        name,
        template,
        detail,
        value,
        keys: &[],
        other_keys: Value::Other,
        field_keys: false,
    }
}

const fn options(
    name: &'static str,
    template: &'static str,
    detail: &'static str,
    keys: &'static [Key],
) -> Stage {
    Stage {
        keys,
        ..plain(name, template, detail, Value::Other)
    }
}

/// Stages whose keys are field paths, each set to an expression.
const fn shaping(name: &'static str, detail: &'static str) -> Stage {
    Stage {
        other_keys: Value::Expression,
        field_keys: true,
        ..plain(name, "{ | }", detail, Value::Other)
    }
}

use Value::{Bool, Collection, Expression, Field, Filter, Keys, Other, Outputs, Pipeline};

/// The stages, most used first, as the stage picker lists them.
pub static STAGES: &[Stage] = &[
    plain("$match", "{ | }", "filter documents", Filter),
    Stage {
        keys: &[key("_id", "|", "group key, e.g. '$field'", Expression)],
        other_keys: Expression,
        ..plain("$group", "{ _id: | }", "group and accumulate", Other)
    },
    shaping("$project", "include, exclude or compute fields"),
    plain("$sort", "{ | }", "sort by fields", Keys),
    plain("$limit", "10|", "keep the first n", Other),
    plain("$skip", "10|", "skip the first n", Other),
    Stage {
        value: Expression,
        ..options(
            "$unwind",
            "'$|'",
            "one document per array element",
            &[
                key("path", "'$|'", "array field", Expression),
                key("includeArrayIndex", "'|'", "field for the index", Other),
                key(
                    "preserveNullAndEmptyArrays",
                    "true|",
                    "keep documents without elements",
                    Bool,
                ),
            ],
        )
    },
    options(
        "$lookup",
        "{ from: '|', localField: '', foreignField: '', as: '' }",
        "join another collection",
        &[
            key("from", "'|'", "collection to join", Collection),
            key("localField", "'|'", "field here", Field),
            key("foreignField", "'|'", "field there", Field),
            key("as", "'|'", "output array field", Other),
            key("let", "{ | }", "variables for the pipeline", Outputs),
            key(
                "pipeline",
                "[|]",
                "stages run on the joined collection",
                Pipeline,
            ),
        ],
    ),
    shaping("$addFields", "add or replace fields"),
    shaping("$set", "add or replace fields"),
    plain("$unset", "'|'", "remove fields", Field),
    plain("$count", "'count|'", "count documents into a field", Other),
    options(
        "$sample",
        "{ size: 10| }",
        "random documents",
        &[key("size", "|", "how many", Other)],
    ),
    plain("$sortByCount", "'$|'", "group, count and sort", Expression),
    options(
        "$replaceRoot",
        "{ newRoot: '$|' }",
        "replace the document",
        &[key("newRoot", "'$|'", "the new document", Expression)],
    ),
    plain("$replaceWith", "'$|'", "replace the document", Expression),
    Stage {
        other_keys: Pipeline,
        ..plain("$facet", "{ |: [] }", "several pipelines at once", Other)
    },
    options(
        "$bucket",
        "{ groupBy: '$|', boundaries: [], default: 'Other', output: { count: { $sum: 1 } } }",
        "group into ranges",
        &[
            key("groupBy", "'$|'", "value to bucket", Expression),
            key("boundaries", "[|]", "range bounds", Other),
            key("default", "'|'", "bucket for the rest", Other),
            key("output", "{ | }", "fields per bucket", Outputs),
        ],
    ),
    options(
        "$bucketAuto",
        "{ groupBy: '$|', buckets: 5 }",
        "group into even ranges",
        &[
            key("groupBy", "'$|'", "value to bucket", Expression),
            key("buckets", "|", "how many", Other),
            key("output", "{ | }", "fields per bucket", Outputs),
            key("granularity", "'|'", "number series, e.g. 'R5'", Other),
        ],
    ),
    options(
        "$graphLookup",
        "{ from: '|', startWith: '$', connectFromField: '', connectToField: '', as: '' }",
        "recursive join",
        &[
            key("from", "'|'", "collection to search", Collection),
            key("startWith", "'$|'", "first value", Expression),
            key("connectFromField", "'|'", "field to follow", Field),
            key("connectToField", "'|'", "field it matches", Field),
            key("as", "'|'", "output array field", Other),
            key("maxDepth", "|", "deepest level", Other),
            key("depthField", "'|'", "field for the depth", Other),
            key(
                "restrictSearchWithMatch",
                "{ | }",
                "filter on the search",
                Filter,
            ),
        ],
    ),
    options(
        "$unionWith",
        "{ coll: '|', pipeline: [] }",
        "add another collection's documents",
        &[
            key("coll", "'|'", "collection", Collection),
            key("pipeline", "[|]", "stages run on it", Pipeline),
        ],
    ),
    plain(
        "$redact",
        "{ $cond: { if: |, then: '$$DESCEND', else: '$$PRUNE' } }",
        "keep or prune by content",
        Expression,
    ),
    options(
        "$setWindowFields",
        "{ partitionBy: '$|', sortBy: {}, output: {} }",
        "window functions",
        &[
            key("partitionBy", "'$|'", "group key", Expression),
            key("sortBy", "{ | }", "order in a partition", Keys),
            key("output", "{ | }", "computed fields", Outputs),
        ],
    ),
    options(
        "$densify",
        "{ field: '|', range: { step: 1, bounds: 'full' } }",
        "fill gaps in a sequence",
        &[
            key("field", "'|'", "field to densify", Field),
            key("partitionByFields", "[|]", "fields to group by", Field),
            key(
                "range",
                "{ step: |, bounds: 'full' }",
                "step and bounds",
                Other,
            ),
        ],
    ),
    options(
        "$fill",
        "{ output: { | } }",
        "fill null and missing values",
        &[
            key("partitionBy", "'$|'", "group key", Expression),
            key("partitionByFields", "[|]", "fields to group by", Field),
            key("sortBy", "{ | }", "order in a partition", Keys),
            key("output", "{ | }", "fields and how to fill them", Other),
        ],
    ),
    options(
        "$geoNear",
        "{ near: { type: 'Point', coordinates: [|] }, distanceField: 'distance' }",
        "sort by distance",
        &[
            key(
                "near",
                "{ type: 'Point', coordinates: [|] }",
                "point",
                Other,
            ),
            key("distanceField", "'|'", "field for the distance", Other),
            key("spherical", "true|", "sphere geometry", Bool),
            key("query", "{ | }", "filter", Filter),
            key("key", "'|'", "geospatial index field", Field),
            key("maxDistance", "|", "farthest", Other),
            key("minDistance", "|", "nearest", Other),
            key(
                "includeLocs",
                "'|'",
                "field for the matched location",
                Other,
            ),
            key("distanceMultiplier", "|", "scales distances", Other),
        ],
    ),
    Stage {
        value: Collection,
        ..options(
            "$out",
            "'|'",
            "write results to a collection",
            &[
                key("db", "'|'", "database", Other),
                key("coll", "'|'", "collection", Collection),
            ],
        )
    },
    options(
        "$merge",
        "{ into: '|' }",
        "merge results into a collection",
        &[
            key("into", "'|'", "collection", Collection),
            key("on", "'|'", "fields that identify a document", Field),
            key("let", "{ | }", "variables for whenMatched", Outputs),
            key(
                "whenMatched",
                "'|'",
                "'replace', 'merge', 'keepExisting' or 'fail'",
                Other,
            ),
            key(
                "whenNotMatched",
                "'|'",
                "'insert', 'discard' or 'fail'",
                Other,
            ),
        ],
    ),
    options(
        "$collStats",
        "{ count: {} }",
        "collection statistics",
        &[
            key("latencyStats", "{ histograms: true| }", "latency", Other),
            key("storageStats", "{|}", "storage", Other),
            key("count", "{|}", "document count", Other),
            key("queryExecStats", "{|}", "query statistics", Other),
        ],
    ),
    plain("$indexStats", "{|}", "index usage", Other),
    plain(
        "$search",
        "{ index: 'default', text: { query: '|', path: '' } }",
        "Atlas Search",
        Other,
    ),
    plain(
        "$searchMeta",
        "{ index: 'default', count: { type: 'total' } }",
        "Atlas Search metadata",
        Other,
    ),
    plain(
        "$vectorSearch",
        "{ index: '|', path: '', queryVector: [], numCandidates: 100, limit: 10 }",
        "Atlas Vector Search",
        Other,
    ),
];

/// Expression operators: name, what follows the colon (`|` marks the
/// cursor) and what it does.
pub(crate) static EXPRESSION_OPERATORS: &[(&str, &str, &str)] = &[
    // Accumulators.
    ("$sum", "|", "sum"),
    ("$avg", "|", "average"),
    ("$min", "|", "smallest"),
    ("$max", "|", "largest"),
    ("$first", "|", "first value"),
    ("$last", "|", "last value"),
    ("$push", "|", "array of values"),
    ("$addToSet", "|", "array of distinct values"),
    ("$count", "{}|", "number of documents"),
    ("$mergeObjects", "|", "merge documents"),
    ("$stdDevPop", "|", "population standard deviation"),
    ("$stdDevSamp", "|", "sample standard deviation"),
    ("$top", "{ sortBy: { | }, output: '' }", "first by sort"),
    ("$bottom", "{ sortBy: { | }, output: '' }", "last by sort"),
    (
        "$topN",
        "{ n: |, sortBy: {}, output: '' }",
        "first n by sort",
    ),
    (
        "$bottomN",
        "{ n: |, sortBy: {}, output: '' }",
        "last n by sort",
    ),
    ("$firstN", "{ n: |, input: '' }", "first n values"),
    ("$lastN", "{ n: |, input: '' }", "last n values"),
    ("$maxN", "{ n: |, input: '' }", "n largest"),
    ("$minN", "{ n: |, input: '' }", "n smallest"),
    (
        "$median",
        "{ input: '$|', method: 'approximate' }",
        "median",
    ),
    (
        "$percentile",
        "{ input: '$|', p: [0.5], method: 'approximate' }",
        "percentiles",
    ),
    // Arithmetic.
    ("$add", "[|]", "add numbers or to a date"),
    ("$subtract", "[|]", "subtract"),
    ("$multiply", "[|]", "multiply"),
    ("$divide", "[|]", "divide"),
    ("$mod", "[|]", "remainder"),
    ("$abs", "|", "absolute value"),
    ("$ceil", "|", "round up"),
    ("$floor", "|", "round down"),
    ("$round", "[|]", "round to places"),
    ("$trunc", "[|]", "truncate to places"),
    ("$sqrt", "|", "square root"),
    ("$pow", "[|]", "power"),
    ("$exp", "|", "e to the power"),
    ("$ln", "|", "natural log"),
    ("$log", "[|]", "log in a base"),
    ("$log10", "|", "log base 10"),
    // Comparison and logic.
    ("$eq", "[|]", "equal"),
    ("$ne", "[|]", "not equal"),
    ("$gt", "[|]", "greater than"),
    ("$gte", "[|]", "greater than or equal"),
    ("$lt", "[|]", "less than"),
    ("$lte", "[|]", "less than or equal"),
    ("$cmp", "[|]", "compare: -1, 0 or 1"),
    ("$and", "[|]", "all true"),
    ("$or", "[|]", "any true"),
    ("$not", "[|]", "negate"),
    ("$cond", "{ if: |, then: , else:  }", "if, then, else"),
    ("$ifNull", "[|]", "first value that isn't null"),
    (
        "$switch",
        "{ branches: [{ case: |, then:  }], default:  }",
        "first matching case",
    ),
    // Strings.
    ("$concat", "[|]", "join strings"),
    ("$substrCP", "[|]", "substring by code points"),
    ("$substrBytes", "[|]", "substring by bytes"),
    ("$toLower", "|", "lowercase"),
    ("$toUpper", "|", "uppercase"),
    ("$trim", "{ input: | }", "trim whitespace"),
    ("$ltrim", "{ input: | }", "trim the start"),
    ("$rtrim", "{ input: | }", "trim the end"),
    ("$split", "[|]", "split by a delimiter"),
    ("$strLenCP", "|", "length in code points"),
    ("$strcasecmp", "[|]", "compare ignoring case"),
    ("$indexOfCP", "[|]", "position of a substring"),
    ("$regexMatch", "{ input: |, regex: // }", "matches a regex"),
    ("$regexFind", "{ input: |, regex: // }", "first regex match"),
    (
        "$regexFindAll",
        "{ input: |, regex: // }",
        "all regex matches",
    ),
    (
        "$replaceOne",
        "{ input: |, find: '', replacement: '' }",
        "replace the first match",
    ),
    (
        "$replaceAll",
        "{ input: |, find: '', replacement: '' }",
        "replace every match",
    ),
    // Arrays.
    ("$arrayElemAt", "[|]", "element at an index"),
    ("$concatArrays", "[|]", "join arrays"),
    (
        "$filter",
        "{ input: |, as: 'item', cond:  }",
        "keep matching elements",
    ),
    (
        "$map",
        "{ input: |, as: 'item', in:  }",
        "transform each element",
    ),
    (
        "$reduce",
        "{ input: |, initialValue: , in:  }",
        "fold into one value",
    ),
    ("$size", "|", "array length"),
    ("$in", "[|]", "value in an array"),
    ("$indexOfArray", "[|]", "position of a value"),
    ("$isArray", "|", "is an array"),
    ("$reverseArray", "|", "reverse"),
    ("$slice", "[|]", "part of an array"),
    ("$sortArray", "{ input: |, sortBy: 1 }", "sort an array"),
    ("$zip", "{ inputs: [|] }", "merge arrays element-wise"),
    ("$range", "[|]", "array of numbers"),
    ("$arrayToObject", "|", "array of pairs to a document"),
    ("$objectToArray", "|", "document to an array of pairs"),
    ("$allElementsTrue", "[|]", "every element true"),
    ("$anyElementTrue", "[|]", "some element true"),
    ("$setUnion", "[|]", "union"),
    ("$setIntersection", "[|]", "intersection"),
    ("$setDifference", "[|]", "difference"),
    ("$setEquals", "[|]", "same elements"),
    ("$setIsSubset", "[|]", "subset"),
    // Dates.
    (
        "$dateToString",
        "{ format: '%Y-%m-%d', date: | }",
        "format a date",
    ),
    ("$dateFromString", "{ dateString: | }", "parse a date"),
    (
        "$dateAdd",
        "{ startDate: |, unit: 'day', amount: 1 }",
        "add time",
    ),
    (
        "$dateSubtract",
        "{ startDate: |, unit: 'day', amount: 1 }",
        "subtract time",
    ),
    (
        "$dateDiff",
        "{ startDate: |, endDate: , unit: 'day' }",
        "time between dates",
    ),
    (
        "$dateTrunc",
        "{ date: |, unit: 'day' }",
        "round a date down",
    ),
    ("$dateToParts", "{ date: | }", "date to its parts"),
    ("$dateFromParts", "{ year: | }", "date from parts"),
    ("$year", "|", "year"),
    ("$month", "|", "month, 1–12"),
    ("$week", "|", "week, 0–53"),
    ("$dayOfMonth", "|", "day of the month"),
    ("$dayOfWeek", "|", "day of the week, 1 = Sunday"),
    ("$dayOfYear", "|", "day of the year"),
    ("$hour", "|", "hour"),
    ("$minute", "|", "minute"),
    ("$second", "|", "second"),
    ("$millisecond", "|", "millisecond"),
    ("$isoWeek", "|", "ISO week"),
    ("$isoWeekYear", "|", "ISO week-numbering year"),
    ("$isoDayOfWeek", "|", "ISO day of the week, 1 = Monday"),
    // Types.
    ("$type", "|", "BSON type name"),
    (
        "$convert",
        "{ input: |, to: 'string' }",
        "convert with fallbacks",
    ),
    ("$toString", "|", "to string"),
    ("$toInt", "|", "to 32-bit integer"),
    ("$toLong", "|", "to 64-bit integer"),
    ("$toDouble", "|", "to double"),
    ("$toDecimal", "|", "to decimal"),
    ("$toBool", "|", "to boolean"),
    ("$toDate", "|", "to date"),
    ("$toObjectId", "|", "to ObjectId"),
    ("$isNumber", "|", "is a number"),
    // Documents and variables.
    (
        "$getField",
        "{ field: '|', input: '$$ROOT' }",
        "field by name",
    ),
    (
        "$setField",
        "{ field: '|', input: '$$ROOT', value:  }",
        "set a field by name",
    ),
    ("$literal", "|", "value as is, not an expression"),
    ("$let", "{ vars: { | }, in:  }", "local variables"),
    ("$rand", "{}|", "random number from 0 to 1"),
    ("$meta", "'|'", "metadata, e.g. 'textScore'"),
    // Window functions.
    ("$rank", "{}|", "rank in the partition"),
    ("$denseRank", "{}|", "rank without gaps"),
    ("$documentNumber", "{}|", "position in the partition"),
    (
        "$shift",
        "{ output: |, by: 1 }",
        "value from another document",
    ),
];

/// Variables every expression can use.
pub(crate) static SYSTEM_VARIABLES: &[(&str, &str)] = &[
    ("$$ROOT", "the whole document"),
    ("$$CURRENT", "the current document"),
    ("$$NOW", "the current time"),
    ("$$REMOVE", "removes the field"),
    ("$$DESCEND", "$redact: look inside"),
    ("$$PRUNE", "$redact: drop"),
    ("$$KEEP", "$redact: keep all"),
];

/// One stage as a document, e.g. `{ $limit: 10 }` from `$limit` and `10`.
pub fn stage_document(name: &str, body: &str) -> Result<Document, Vec<ParseError>> {
    let value = crate::parse_value(body)?;
    let mut stage = Document::new();
    stage.insert(name, value);
    Ok(stage)
}

/// The stages of a pipeline written as an array, e.g. pasted from mongosh,
/// as each stage's name and the text of its value. The text is kept as
/// written, comments too, with the indent common to its lines removed.
pub fn split_pipeline(text: &str) -> Result<Vec<(String, String)>, Vec<ParseError>> {
    let error = |message: &str, span| {
        vec![ParseError {
            message: message.to_owned(),
            span,
        }]
    };
    // Reports bad values like `ObjectId('x')` before splitting.
    crate::parse_value(text)?;
    let parsed = parse(text);
    let Some(root) = parsed.root else {
        return Err(error("Enter a pipeline.", 0..text.len()));
    };
    let NodeKind::Array(items) = root.kind else {
        return Err(error(
            "Expected a pipeline in brackets, e.g. [{ $match: {} }].",
            root.span,
        ));
    };
    items
        .into_iter()
        .map(|item| match item.kind {
            NodeKind::Object(members) if members.len() == 1 && members[0].key.starts_with('$') => {
                let member = &members[0];
                Ok((member.key.clone(), dedent(text, member.value.span.clone())))
            }
            _ => Err(error(
                "Each stage must be a document with one stage name, e.g. { $match: {} }.",
                item.span,
            )),
        })
        .collect()
}

/// The text at `span` without the indent its later lines have in common.
fn dedent(text: &str, span: std::ops::Range<usize>) -> String {
    let value = &text[span];
    let indent = value
        .lines()
        .skip(1)
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.len() - line.trim_start().len())
        .min()
        .unwrap_or(0);
    let mut lines = value.lines();
    let mut out = lines.next().unwrap_or_default().to_owned();
    for line in lines {
        out.push('\n');
        out.push_str(line.get(indent..).unwrap_or(line.trim_start()));
    }
    out
}

/// A pipeline as text, one stage per line, each value as written.
pub fn join_pipeline<'a>(stages: impl IntoIterator<Item = (&'a str, &'a str)>) -> String {
    let mut out = String::from("[");
    let mut any = false;
    for (name, body) in stages {
        out.push_str(if any { ",\n  { " } else { "\n  { " });
        any = true;
        crate::format::write_key(&mut out, name);
        out.push_str(": ");
        out.push_str(&body.trim().replace('\n', "\n  "));
        out.push_str(" }");
    }
    out.push_str(if any { "\n]" } else { "]" });
    out
}

#[cfg(test)]
mod tests {
    use bson::doc;

    use super::*;

    #[test]
    fn every_template_parses_once_filled_in() {
        for stage in STAGES {
            let parses =
                |fill: &str| crate::parse_value(&stage.template.replace('|', fill)).is_ok();
            assert!(parses("") || parses("1"), "{} template", stage.name);
            assert_eq!(super::stage(stage.name), Some(stage));
        }
    }

    #[test]
    fn writes_only_for_out_and_merge() {
        let writing: Vec<_> = STAGES
            .iter()
            .filter(|s| s.writes())
            .map(|s| s.name)
            .collect();
        assert_eq!(writing, ["$out", "$merge"]);
    }

    #[test]
    fn builds_a_stage_document() {
        assert_eq!(
            stage_document("$limit", "10").unwrap(),
            doc! { "$limit": 10 }
        );
        assert_eq!(
            stage_document("$group", "{ _id: '$city', n: { $sum: 1 } }").unwrap(),
            doc! { "$group": { "_id": "$city", "n": { "$sum": 1 } } }
        );
        assert!(stage_document("$match", "{ a: }").is_err());
    }

    #[test]
    fn splits_a_pipeline_keeping_its_text() {
        let text = "[\n  { $match: {\n      a: 1, // one\n    } },\n  { $limit: 5 },\n]";
        assert_eq!(
            split_pipeline(text).unwrap(),
            [
                ("$match".to_owned(), "{\n  a: 1, // one\n}".to_owned()),
                ("$limit".to_owned(), "5".to_owned()),
            ]
        );
        assert_eq!(split_pipeline("[]").unwrap(), []);
    }

    #[test]
    fn rejects_what_is_not_a_pipeline() {
        let message = |text: &str| split_pipeline(text).unwrap_err()[0].message.clone();
        assert!(message("{ $match: {} }").starts_with("Expected a pipeline"));
        assert!(message("[{ a: 1 }]").starts_with("Each stage must be"));
        assert!(message("[{ $match: {}, $limit: 1 }]").starts_with("Each stage must be"));
        assert_eq!(message(""), "Enter a value.");
        assert!(split_pipeline("[{ $match: { _id: ObjectId('x') } }]").is_err());
    }

    #[test]
    fn joins_stages_into_a_pipeline() {
        let text = join_pipeline([("$match", "{\n  a: 1\n}"), ("$limit", "5")]);
        assert_eq!(
            text,
            "[\n  { $match: {\n    a: 1\n  } },\n  { $limit: 5 }\n]"
        );
        let stages = split_pipeline(&text).unwrap();
        assert_eq!(stages[0].1, "{\n  a: 1\n}");
        assert_eq!(join_pipeline([]), "[]");
    }
}
