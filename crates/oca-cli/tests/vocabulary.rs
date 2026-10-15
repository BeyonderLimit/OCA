//! Rule 7 (vocabulary firewall): **no Git vocabulary in user-facing
//! strings.** The UX says "Snapshots", never "commits".
//!
//! Scans every string literal and doc comment (clap derives `--help` from
//! doc comments) in all crate sources. Plain `//` comments are excluded —
//! they never reach a user.

use std::path::PathBuf;

/// Unambiguous Git terms. Generic English words that happen to be Git verbs
/// (push, pull, tag, revert) are deliberately not banned.
const BANNED: &[&str] = &[
    "commit",
    "commits",
    "hash",
    "hashes",
    "branch",
    "branches",
    "stash",
    "blame",
    "cherry-pick",
];

/// `HEAD` is banned case-sensitively; lowercase "head" is ordinary English.
const BANNED_CASE_SENSITIVE: &[&str] = &["HEAD"];

/// Extract string literals (handling escapes) and `///`/`//!` doc comments.
fn user_facing_fragments(src: &str) -> Vec<String> {
    let mut out = Vec::new();

    // String literals.
    let mut in_str = false;
    let mut escaped = false;
    let mut cur = String::new();
    for c in src.chars() {
        if in_str {
            if escaped {
                escaped = false;
                cur.push(c);
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_str = false;
                out.push(std::mem::take(&mut cur));
            } else {
                cur.push(c);
            }
        } else if c == '"' {
            in_str = true;
        }
    }

    // Doc comments (help text and rustdoc).
    for line in src.lines() {
        let t = line.trim_start();
        if let Some(rest) = t.strip_prefix("//!") {
            out.push(rest.to_string());
        } else if let Some(rest) = t.strip_prefix("///") {
            out.push(rest.to_string());
        }
    }

    out
}

/// Find `word` at an alphanumeric word boundary.
fn find_word(hay: &str, word: &str, case_sensitive: bool) -> Option<usize> {
    let hay = if case_sensitive {
        hay.to_string()
    } else {
        hay.to_lowercase()
    };
    let word = if case_sensitive {
        word.to_string()
    } else {
        word.to_lowercase()
    };
    let bytes = hay.as_bytes();
    let mut from = 0;
    while let Some(pos) = hay[from..].find(&word) {
        let start = from + pos;
        let end = start + word.len();
        let before_ok = start == 0 || !bytes[start - 1].is_ascii_alphanumeric();
        let after_ok = end == bytes.len() || !bytes[end].is_ascii_alphanumeric();
        if before_ok && after_ok {
            return Some(start);
        }
        from = end;
    }
    None
}

/// The scanner must actually work, or the firewall test is vacuous.
#[test]
fn scanner_detects_banned_words_and_respects_boundaries() {
    let frags = user_facing_fragments(
        "fn f() { println!(\"please commit now\"); }\n/// HEAD is not user-facing wording",
    );
    assert!(
        frags
            .iter()
            .any(|f| find_word(f, "commit", false).is_some()),
        "literal extraction missed the banned word: {frags:?}"
    );
    assert!(frags.iter().any(|f| find_word(f, "HEAD", true).is_some()));
    // Word boundaries: inside a longer word it is not the Git term.
    assert!(find_word("recommitted", "commit", false).is_none());
    assert!(find_word("hashing out a plan", "hash", false).is_none());
    // Plain // comments are excluded (they never reach a user).
    let only_comment = user_facing_fragments("// let's commit to the plan\nfn g() {}");
    assert!(
        !only_comment
            .iter()
            .any(|f| find_word(f, "commit", false).is_some())
    );
}

#[test]
fn no_git_vocabulary_in_user_facing_strings() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let crates_dir = manifest.parent().unwrap().parent().unwrap().join("crates");
    let mut violations = Vec::new();
    let mut scanned_files = 0usize;
    let mut scanned_fragments = 0usize;

    let entries = std::fs::read_dir(&crates_dir).expect("crates/ directory");
    for entry in entries.flatten() {
        let src_dir = entry.path().join("src");
        if !src_dir.is_dir() {
            continue;
        }
        let mut walk = vec![src_dir.clone()];
        while let Some(dir) = walk.pop() {
            for file in std::fs::read_dir(&dir).unwrap().flatten() {
                let path = file.path();
                if path.is_dir() {
                    walk.push(path);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    let text = std::fs::read_to_string(&path).unwrap();
                    scanned_files += 1;
                    for frag in user_facing_fragments(&text) {
                        scanned_fragments += 1;
                        for word in BANNED {
                            if find_word(&frag, word, false).is_some() {
                                violations.push(format!("{}: {word} in {frag:?}", path.display()));
                            }
                        }
                        for word in BANNED_CASE_SENSITIVE {
                            if find_word(&frag, word, true).is_some() {
                                violations.push(format!("{}: {word} in {frag:?}", path.display()));
                            }
                        }
                    }
                }
            }
        }
    }

    assert!(
        violations.is_empty(),
        "Git vocabulary leaked into user-facing strings (AGENTS rule 7):\n{}",
        violations.join("\n")
    );
    // Floor: prove the walk read real sources (an empty scan would pass).
    assert!(
        scanned_files > 10 && scanned_fragments > 200,
        "scanner read only {scanned_files} files / {scanned_fragments} fragments — walk is broken"
    );
}
