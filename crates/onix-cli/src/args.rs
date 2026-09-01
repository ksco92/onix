//! Argument parsing for the `onix diff` subcommand: [`DiffArgs`] and how to
//! get one from `std::env::args()` (or an `ONIX_MAX_DEPTH` fallback), with
//! no I/O or `onix_core` calls of its own — see `super::run` for what
//! happens with a parsed [`DiffArgs`].

/// Usage text printed (to stderr) on any argument-parsing error.
pub(crate) const USAGE: &str =
    "usage: onix diff <a.json> <b.json> [--max-depth N] [--ignore-order] [--timing]";

/// Environment variable used as the `--max-depth` default when the flag is
/// not passed (see [`resolve_default_max_depth`]).
const MAX_DEPTH_ENV_VAR: &str = "ONIX_MAX_DEPTH";

/// The parsed form of `onix diff <a> <b> [--max-depth N] [--ignore-order]
/// [--timing]`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct DiffArgs {
    pub(crate) a_path: String,
    pub(crate) b_path: String,
    pub(crate) max_depth: Option<usize>,
    pub(crate) ignore_order: bool,
    pub(crate) timing: bool,
}

/// Parses the arguments to the `diff` subcommand (everything after the
/// `diff` token itself is not included here; see [`parse_args`]).
///
/// `max_depth` is `None` when `--max-depth` was not passed, so the caller
/// can fall back to [`resolve_default_max_depth`] — keeping "was it passed
/// explicitly" and "what's the effective default" as separate concerns.
pub(crate) fn parse_diff_args(args: &[String]) -> Result<DiffArgs, String> {
    let mut positionals: Vec<&str> = Vec::new();
    let mut max_depth: Option<usize> = None;
    let mut ignore_order = false;
    let mut timing = false;

    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--max-depth" => {
                let value = iter
                    .next()
                    .ok_or_else(|| "--max-depth requires a value".to_string())?;
                max_depth = Some(
                    value
                        .parse::<usize>()
                        .map_err(|_| format!("invalid --max-depth value: {value}"))?,
                );
            }
            "--ignore-order" => ignore_order = true,
            "--timing" => timing = true,
            other if other.starts_with("--") => return Err(format!("unknown flag: {other}")),
            other => positionals.push(other),
        }
    }

    let [a_path, b_path] = positionals.as_slice() else {
        return Err(format!(
            "expected 2 positional arguments (a.json b.json), got {}",
            positionals.len()
        ));
    };

    Ok(DiffArgs {
        a_path: (*a_path).to_string(),
        b_path: (*b_path).to_string(),
        max_depth,
        ignore_order,
        timing,
    })
}

/// Parses the full CLI argument list (`args[0]` must be the `diff`
/// subcommand name).
pub(crate) fn parse_args(args: &[String]) -> Result<DiffArgs, String> {
    match args.first() {
        None => Err("missing subcommand".to_string()),
        Some(cmd) if cmd == "diff" => parse_diff_args(&args[1..]),
        Some(other) => Err(format!("unknown subcommand: {other}")),
    }
}

/// The pure parsing logic behind [`resolve_default_max_depth`]: `None` (the
/// environment variable was unset) or an unparseable value both fall back to
/// [`onix_core::DEFAULT_MAX_DEPTH`]; a parseable value is used as-is.
///
/// Split out from [`resolve_default_max_depth`] purely so this logic is
/// unit-testable without mutating real process environment state — `std::env
/// ::set_var`/`remove_var` are `unsafe` as of Rust 2024 (soundness hazard in
/// multi-threaded processes), and this workspace forbids `unsafe` outright.
/// The real end-to-end "`ONIX_MAX_DEPTH` is respected" behavior is covered by
/// `tests/cli.rs`, which sets the variable safely on a spawned subprocess via
/// [`std::process::Command::env`].
pub(crate) fn parse_max_depth_env_value(value: Option<&str>) -> usize {
    value
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(onix_core::DEFAULT_MAX_DEPTH)
}

/// Resolves the effective `--max-depth`: the flag value if passed, else
/// `ONIX_MAX_DEPTH` from the environment if set and parseable as a `usize`,
/// else [`onix_core::DEFAULT_MAX_DEPTH`] (see [`parse_max_depth_env_value`]).
///
/// This ambient-environment default is a deliberate CLI-only convenience —
/// `onix-core` itself stays a pure function with no environment
/// dependence; only the binary reads `ONIX_MAX_DEPTH`.
pub(crate) fn resolve_default_max_depth() -> usize {
    parse_max_depth_env_value(std::env::var(MAX_DEPTH_ENV_VAR).ok().as_deref())
}
