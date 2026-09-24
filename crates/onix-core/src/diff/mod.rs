//! The diff engine's entry point: recursive type-dispatch over two
//! `Value` trees into a `Report`; list diffing is `docs/design/list-diff.md`.
//! Submodules: `options` (public API), `dispatch` (traversal core),
//! `scalar` (leaf comparison), `array` (list diffing), `object` (dict diffing).

mod array;
mod dispatch;
mod object;
mod options;
mod scalar;
mod set;

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

pub use options::{
    DEFAULT_MAX_DEPTH, DiffOptions, Resolution, Resolver, diff, diff_with_max_depth,
    diff_with_options, diff_with_resolver,
};

pub(crate) use array::array_diff;
pub(crate) use dispatch::{
    check_map_depth, check_traversal_depth, check_value_depth, deeper_than, diff_at, scoped,
    values_equal,
};
pub(crate) use object::object_diff;
#[cfg(test)]
pub(crate) use options::diff_with_options_memo;
pub(crate) use scalar::{
    datetime_diff, effective_type_name, normalized_pair, numbers_equal, numeric_diff,
    python_type_name, scalar_diff, type_change_report,
};
pub(crate) use set::set_diff;
