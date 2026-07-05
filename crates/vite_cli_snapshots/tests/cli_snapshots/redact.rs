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
// Only v-prefixed versions are masked: tool and runtime banners all print
// that form (`vite v7.3.2`, `vp v0.2.2`, `Node.js v24.18.0`) and churn on
// every dep bump, while bare semver literals (`app-1.0.0.tgz`,
// `"vitest": "4.0.13"`) are user-controlled values that snapshots must be
// able to assert.
static VERSION_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"\bv\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?\b").unwrap()
});
static THREAD_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"\d+ threads").unwrap());
// Output bytes differ across OSes (line endings, embedded paths), so byte
// sizes and content-derived asset hashes can never be part of a shared
// snapshot. The unit is kept ("<size> kB"): it only changes when content
// crosses a magnitude boundary, which is real signal. Durations stay fully
// masked instead, because their unit flips with timing (999ms vs 1.00s).
static SIZE_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"\b\d+(?:\.\d+)?(\s?)(B|kB|KB|KiB|MB|MiB|GB|GiB)\b").unwrap()
});
static ASSET_HASH_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"-([A-Za-z0-9_-]{8})\.(js|mjs|cjs|css)\b").unwrap());
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
fn redact_string(s: &mut String, redactions: &[(&str, &str)], normalize_separators: bool) {
    use cow_utils::CowUtils as _;
    for (from, to) in redactions {
        if let Cow::Owned(replaced) = s.as_str().cow_replace(from, to) {
            *s = replaced;
        }
    }
    // Normalize path separators unconditionally on Windows: tools print
    // OS-native separators for relative paths too (`src\index.ts`), which no
    // absolute-path redaction pair ever matches. Debug-formatted paths escape
    // separators (`\\`); collapse those BEFORE converting so they cannot
    // become `//` (collapsing afterwards would also mangle `https://` URLs).
    // Skipped for formatted-snapshot captures, whose literal escape
    // renderings (`\x1b[...`) must survive byte for byte.
    if cfg!(windows) && normalize_separators {
        while s.contains("\\\\") {
            if let Cow::Owned(replaced) = s.as_str().cow_replace("\\\\", "\\") {
                *s = replaced;
            }
        }
        if let Cow::Owned(replaced) = s.as_str().cow_replace('\\', "/") {
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
    // Windows children also print forward-slash forms of absolute paths
    // (file:// URLs, JS stack frames); the separator-normalization pass runs
    // after redaction, so those need their own variants.
    let slashed = path.cow_replace('\\', "/").into_owned();
    if slashed != path && !variants.iter().any(|(v, _)| *v == slashed) {
        variants.push((slashed, label));
    }
    let stripped_slashed = stripped.cow_replace('\\', "/").into_owned();
    if stripped_slashed != stripped && !variants.iter().any(|(v, _)| *v == stripped_slashed) {
        variants.push((stripped_slashed, label));
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
pub fn redact_output(
    mut output: String,
    paths: &[(&str, &'static str)],
    normalize_separators: bool,
) -> String {
    // ConPTY repaints rows padded to the full grid width with explicit
    // spaces when a second console client attaches to the terminal. Trailing
    // blanks are never meaningful in a rendered grid, so trim every row on
    // every platform, keeping one snapshot valid across OSes (Unix captures
    // already come trimmed from vt100, so this is a no-op there).
    if output.lines().any(|line| line.ends_with([' ', '\t'])) {
        let had_trailing_newline = output.ends_with('\n');
        output = output.lines().map(str::trim_end).collect::<Vec<_>>().join("\n");
        if had_trailing_newline {
            output.push('\n');
        }
    }

    let mut redactions: Vec<(String, &'static str)> = Vec::new();
    for (path, label) in paths {
        redactions.extend(path_variants(path, label));
    }
    let borrowed: Vec<(&str, &str)> =
        redactions.iter().map(|(from, to)| (from.as_str(), *to)).collect();
    redact_string(&mut output, &borrowed, normalize_separators);

    // Redact UUIDs to "<uuid>"
    output = UUID_RE.replace_all(&output, "<uuid>").into_owned();

    // Redact durations like "0ns", "123ms" or "1.23s" to "<duration>".
    // Runs before version redaction so "1.23s" never half-matches as a version.
    output = DURATION_RE.replace_all(&output, "<duration>").into_owned();

    // Redact semver-shaped versions (bundled tool versions, Node versions).
    output = VERSION_RE.replace_all(&output, "<version>").into_owned();

    // Redact thread counts like "16 threads" to "<n> threads"
    output = THREAD_RE.replace_all(&output, "<n> threads").into_owned();

    // Redact byte-size numbers like "0.12 kB" to "<size> kB" (unit kept)
    output = SIZE_RE.replace_all(&output, "<size>${1}${2}").into_owned();

    // Redact content-hash suffixes in emitted asset names
    // (`index-Dra_-aT4.js` to `index-<hash>.js`). Requires a digit or an
    // uppercase letter in the hash so ordinary 8-letter words in filenames
    // (`some-tsconfig.js`) survive.
    output = ASSET_HASH_RE
        .replace_all(&output, |caps: &regex::Captures| {
            let hash = &caps[1];
            if hash.bytes().any(|b| b.is_ascii_digit() || b.is_ascii_uppercase()) {
                format!("-<hash>.{}", &caps[2])
            } else {
                caps[0].to_owned()
            }
        })
        .into_owned();

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
