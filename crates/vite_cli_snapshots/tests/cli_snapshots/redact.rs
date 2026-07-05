//! Normalization of captured terminal screens before they enter a snapshot.
//!
//! Deliberately minimal compared to the old snap-test `replaceUnstableOutput`:
//! grid rendering already removes ANSI noise, spinner frames, and
//! stdout/stderr interleaving, so every rule here should correspond to a real
//! source of nondeterminism (paths, durations, versions, machine parallelism).

use std::{borrow::Cow, sync::LazyLock};

// Compiled once per run: redaction runs on every snapshotted step, and regex
// compilation dominates matching cost at that frequency.
static UUID_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}").unwrap()
});
static DURATION_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"\b\d+(\.\d+)?(ns|µs|ms|s)\b").unwrap());
static VERSION_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"\bv?\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?\b").unwrap()
});
static THREAD_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"\d+ threads").unwrap());
static NODE_WARNING_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"(?m)^\(node:\d+\) ExperimentalWarning:.*\n?").unwrap());
static NODE_TRACE_WARNING_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r"(?m)^\(Use `node --trace-warnings \.\.\.` to show where the warning was created\)\n?",
    )
    .unwrap()
});

#[expect(
    clippy::disallowed_types,
    reason = "String mutation required by regex replace and cow_replace APIs"
)]
fn redact_string(s: &mut String, redactions: &[(&str, &str)]) {
    use cow_utils::CowUtils as _;
    for (from, to) in redactions {
        if let Cow::Owned(mut replaced) = s.as_str().cow_replace(from, to) {
            if cfg!(windows) {
                // Normalize backslashes to forward slashes on Windows
                replaced = replaced.cow_replace("\\", "/").into_owned();
                // Collapse double slashes that arise when an escaped path separator (\\)
                // is only partially replaced (e.g., Debug-format paths end with \\")
                while replaced.contains("//") {
                    replaced = replaced.cow_replace("//", "/").into_owned();
                }
            }
            *s = replaced;
        }
    }
}

/// Expands one `(path, label)` pair into the variants child processes may
/// print: raw, without the Windows `\\?\` verbatim prefix, and Debug-format
/// escaped (backslashes doubled). Longest variants sort first so partial
/// replacements never leave stray prefixes behind.
#[expect(
    clippy::disallowed_types,
    reason = "String required to own generated path variants for replacement"
)]
fn path_variants(path: &str, label: &'static str) -> Vec<(String, &'static str)> {
    use cow_utils::CowUtils as _;
    let stripped = path.strip_prefix(r"\\?\").unwrap_or(path);
    let mut variants = vec![(path.to_owned(), label)];
    if stripped != path {
        variants.push((stripped.to_owned(), label));
    }
    let escaped = path.cow_replace('\\', r"\\").into_owned();
    if escaped != path {
        variants.insert(0, (escaped, label));
    }
    let stripped_escaped = stripped.cow_replace('\\', r"\\").into_owned();
    if stripped_escaped != stripped && !variants.iter().any(|(v, _)| *v == stripped_escaped) {
        variants.insert(1, (stripped_escaped, label));
    }
    variants
}

/// Redacts a captured screen. `paths` maps machine-specific absolute paths to
/// stable labels, e.g. `(<staged fixture root>, "<workspace>")`,
/// `(<case home>, "<home>")`, `(<repo checkout>, "<repo>")`.
#[expect(
    clippy::disallowed_types,
    reason = "String required by regex replace_all and cow_replace APIs"
)]
pub fn redact_output(mut output: String, paths: &[(&str, &'static str)]) -> String {
    let mut redactions: Vec<(String, &'static str)> = Vec::new();
    for (path, label) in paths {
        redactions.extend(path_variants(path, label));
    }
    let borrowed: Vec<(&str, &str)> =
        redactions.iter().map(|(from, to)| (from.as_str(), *to)).collect();
    redact_string(&mut output, &borrowed);

    // Redact UUIDs to "<uuid>"
    output = UUID_RE.replace_all(&output, "<uuid>").into_owned();

    // Redact durations like "0ns", "123ms" or "1.23s" to "<duration>".
    // Runs before version redaction so "1.23s" never half-matches as a version.
    output = DURATION_RE.replace_all(&output, "<duration>").into_owned();

    // Redact semver-shaped versions (bundled tool versions, Node versions).
    output = VERSION_RE.replace_all(&output, "<version>").into_owned();

    // Redact thread counts like "16 threads" to "<n> threads"
    output = THREAD_RE.replace_all(&output, "<n> threads").into_owned();

    // Remove Node.js experimental warnings (e.g., Type Stripping warnings)
    output = NODE_WARNING_RE.replace_all(&output, "").into_owned();
    output = NODE_TRACE_WARNING_RE.replace_all(&output, "").into_owned();

    // Remove ^C echo that Unix terminal drivers emit when ETX (0x03) is written
    // to the PTY. Windows ConPTY does not echo it.
    {
        use cow_utils::CowUtils as _;
        if let Cow::Owned(replaced) = output.as_str().cow_replace("^C", "") {
            output = replaced;
        }
    }

    // Sort consecutive diagnostic blocks to handle non-deterministic tool output
    // (e.g., oxlint reports warnings in arbitrary order due to multi-threading).
    // Each block starts with "  ! " and ends at the next empty line. Most
    // screens have none, so skip the split/rejoin allocation entirely then.
    if output.contains("  ! ") {
        output = sort_diagnostic_blocks(&output);
    }

    output
}

#[expect(
    clippy::disallowed_types,
    reason = "String return required because join produces a String"
)]
fn sort_diagnostic_blocks(output: &str) -> String {
    let parts: Vec<&str> = output.split('\n').collect();
    let mut result: Vec<&str> = Vec::new();
    let mut i = 0;

    while i < parts.len() {
        if parts[i].starts_with("  ! ") {
            let mut blocks: Vec<Vec<&str>> = Vec::new();

            loop {
                if i >= parts.len() || !parts[i].starts_with("  ! ") {
                    break;
                }
                let mut block: Vec<&str> = Vec::new();
                while i < parts.len() && !parts[i].is_empty() {
                    block.push(parts[i]);
                    i += 1;
                }
                blocks.push(block);
                // Skip the empty line separator between blocks
                if i < parts.len() && parts[i].is_empty() {
                    i += 1;
                }
            }

            blocks.sort();

            // Restore an empty-line separator after every block (`i` never
            // exceeds parts.len(), so the upstream guard here was always
            // true; keep the behavior, drop the misleading condition).
            for block in &blocks {
                result.extend_from_slice(block);
                result.push("");
            }
        } else {
            result.push(parts[i]);
            i += 1;
        }
    }

    result.join("\n")
}
