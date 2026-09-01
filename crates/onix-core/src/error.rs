//! Error type for [`crate::diff::diff`].

use std::fmt;

/// Errors that can occur while diffing two values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Diffing `a`/`b` (without the two inputs resolving as fully equal
    /// first) would need more native recursion than the configured maximum
    /// depth allows ([`crate::diff::diff_with_max_depth`]'s `max_depth`,
    /// default [`crate::diff::DEFAULT_MAX_DEPTH`]). This fires for either
    /// of two related reasons, both measured against the *same* `max_depth`
    /// budget so their native stack usage can never add up to more than
    /// `max_depth` frames total (see
    /// [`crate::diff::diff_with_max_depth`]'s doc for the exact contract):
    ///
    /// - the traversal itself would need to recurse past `max_depth` just
    ///   to *reach* a difference, with no finding's value even in play yet,
    ///   or
    /// - a difference *was* reached, but the value it would record (added,
    ///   removed, changed, or type-changed) is, combined with how deep its
    ///   path already is, nested past the `max_depth` budget remaining at
    ///   that path.
    ///
    /// This is a deliberate, safe stop either way: it replaces a stack
    /// overflow (uncatchable, aborts the process) on adversarial
    /// deeply-nested input with an ordinary, recoverable error. This bound
    /// is temporary scaffolding — see
    /// [`crate::diff::diff_with_max_depth`]'s doc for why: once the
    /// recursive engine is replaced by an iterative work-stack, this
    /// practical depth limit goes away entirely.
    MaxDepthExceeded {
        /// The DeepDiff-style path (e.g. `"root['a']['b']"`) at which the
        /// bound was exceeded — either where the traversal gave up, or
        /// where an over-budget finding's value would have been recorded.
        path: String,
        /// The configured maximum depth that was exceeded.
        max_depth: usize,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::MaxDepthExceeded { path, max_depth } => {
                write!(
                    f,
                    "diffing at {path} would exceed the configured maximum recursion \
                     depth ({max_depth}), counting both the traversal needed to reach it \
                     and the nesting of any value recorded there"
                )
            }
        }
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::Error;

    #[test]
    fn max_depth_exceeded_display_includes_path_and_max_depth() {
        let error = Error::MaxDepthExceeded {
            path: "root['a']['b']".to_string(),
            max_depth: 512,
        };
        let message = error.to_string();
        assert!(message.contains("root['a']['b']"));
        assert!(message.contains("512"));
    }

    #[test]
    fn max_depth_exceeded_is_clonable_and_comparable() {
        let a = Error::MaxDepthExceeded {
            path: "root".to_string(),
            max_depth: 3,
        };
        let b = a.clone();
        assert_eq!(a, b);
    }

    #[test]
    fn max_depth_exceeded_implements_std_error() {
        let error = Error::MaxDepthExceeded {
            path: "root".to_string(),
            max_depth: 3,
        };
        let _: &dyn std::error::Error = &error;
    }
}
