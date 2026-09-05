//! Renders an Arrow [`RecordBatch`] to a JSON array of row objects, for
//! [`crate::TableDiff::to_json`]. Reuses [`crate::row_diff::SideRenderer`],
//! the same renderer [`crate::TableDiff::cells_changed`]'s `old_value`/
//! `new_value` use, so no cell renders two different ways in the crate.
//!
//! [`MAX_JSON_ROWS`] bounds the row count: the JSON holds one object per row
//! and one owned string per non-null cell, so memory is proportional to
//! rows × columns × cell width, uncapped past that row count otherwise. A
//! batch over the cap is refused with [`crate::TableDiffError::TooManyJsonRows`].

use arrow_array::RecordBatch;
use arrow_cast::display::FormatOptions;
use serde_json::{Map, Value as JsonValue};

use crate::error::TableDiffError;
use crate::row_diff::{SideRenderer, cell_is_null};

/// The maximum number of row objects this module will render for one
/// [`RecordBatch`]; see the module doc for what this bounds.
pub const MAX_JSON_ROWS: usize = 10_000;

/// Renders every row of `batch` to a JSON object keyed by column name, or
/// [`TableDiffError::TooManyJsonRows`] if `batch` has more than
/// [`MAX_JSON_ROWS`] rows. A null cell renders as JSON `null`; every other
/// cell renders through [`SideRenderer`], the same renderer
/// [`crate::TableDiff::cells_changed`] uses.
///
/// # Errors
///
/// - [`TableDiffError::TooManyJsonRows`] if `batch.num_rows()` exceeds
///   [`MAX_JSON_ROWS`].
/// - [`TableDiffError::Render`] if a cell cannot be rendered to its
///   canonical string (naming the column), the same failure
///   [`crate::TableDiff::cells_changed`] can raise.
pub(crate) fn rows_to_json(batch: &RecordBatch) -> Result<Vec<JsonValue>, TableDiffError> {
    if batch.num_rows() > MAX_JSON_ROWS {
        return Err(TableDiffError::TooManyJsonRows {
            rows: batch.num_rows(),
            max: MAX_JSON_ROWS,
        });
    }

    let format_options = FormatOptions::default();
    let schema = batch.schema();
    let columns = schema
        .fields()
        .iter()
        .zip(batch.columns())
        .map(|(field, array)| {
            SideRenderer::new(array, &format_options)
                .map(|renderer| (field.name(), array, renderer))
        })
        .collect::<Result<Vec<_>, _>>()?;

    let mut rows = Vec::with_capacity(batch.num_rows());
    for row in 0..batch.num_rows() {
        let mut object = Map::with_capacity(columns.len());
        for (name, array, renderer) in &columns {
            let value = if cell_is_null(array, row) {
                JsonValue::Null
            } else {
                JsonValue::String(renderer.render(row, name)?)
            };
            object.insert((*name).clone(), value);
        }
        rows.push(JsonValue::Object(object));
    }

    Ok(rows)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use arrow_array::{Int64Array, RecordBatch, StringArray};
    use arrow_schema::{DataType, Field, Schema};

    use super::{MAX_JSON_ROWS, rows_to_json};
    use crate::error::TableDiffError;

    fn batch(rows: usize) -> RecordBatch {
        let schema = Arc::new(Schema::new(vec![
            Field::new("id", DataType::Int64, false),
            Field::new("name", DataType::Utf8, true),
        ]));
        let ids: Vec<i64> = (0..rows).map(|i| i64::try_from(i).unwrap()).collect();
        let names: Vec<Option<String>> = (0..rows)
            .map(|i| {
                if i % 2 == 0 {
                    None
                } else {
                    Some(format!("n{i}"))
                }
            })
            .collect();
        RecordBatch::try_new(
            schema,
            vec![
                Arc::new(Int64Array::from(ids)),
                Arc::new(StringArray::from(names)),
            ],
        )
        .unwrap()
    }

    #[test]
    fn renders_one_object_per_row_with_column_keys() {
        let rows = rows_to_json(&batch(2)).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["id"], serde_json::json!("0"));
        assert_eq!(rows[0]["name"], serde_json::Value::Null);
        assert_eq!(rows[1]["id"], serde_json::json!("1"));
        assert_eq!(rows[1]["name"], serde_json::json!("n1"));
    }

    #[test]
    fn empty_batch_renders_no_rows() {
        assert_eq!(
            rows_to_json(&batch(0)).unwrap(),
            Vec::<serde_json::Value>::new()
        );
    }

    #[test]
    fn batch_over_the_cap_is_refused() {
        let error = rows_to_json(&batch(MAX_JSON_ROWS + 1)).unwrap_err();
        assert_eq!(
            error,
            TableDiffError::TooManyJsonRows {
                rows: MAX_JSON_ROWS + 1,
                max: MAX_JSON_ROWS,
            }
        );
    }

    #[test]
    fn batch_at_the_cap_is_accepted() {
        assert!(rows_to_json(&batch(MAX_JSON_ROWS)).is_ok());
    }
}
