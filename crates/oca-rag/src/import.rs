//! Reference-library import (Phase 11): Markdown, plain text, and EPUB
//! become `library/<slug>.md` documents whose frontmatter carries the
//! provenance the A8 amendment demands — source kind, original path, and
//! import date. Files stay canonical (rule 2): the copied document *is*
//! the reference; every index over it is rebuildable from it.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, anyhow};
use oca_markdown::parse;
use oca_model::{Frontmatter, new_id};
use oca_storage::layout::{slugify, write_new};

/// What one import produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Imported {
    /// Project-relative path of the new `library/` document.
    pub rel: PathBuf,
    pub title: String,
    /// `markdown`, `text`, or `epub`.
    pub source_type: &'static str,
    /// The immutable id the copy carries.
    pub id: String,
}

/// Import one file into the project's reference library. The source is
/// read, normalized to Markdown, and written once — the original file is
/// never modified.
pub fn import_file(project_root: &Path, path: &Path) -> Result<Imported> {
    let source_type = source_type(path)?;
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "untitled".into());

    // Normalize to (frontmatter, markdown body).
    let (mut fm, title, body) = match source_type {
        "markdown" => {
            let raw = std::fs::read_to_string(path)
                .with_context(|| format!("read {}", path.display()))?;
            let doc = parse(&raw);
            let title = doc
                .frontmatter
                .get_str("title")
                .filter(|t| !t.is_empty())
                .or_else(|| heading_title(doc.body()))
                .unwrap_or_else(|| stem.clone());
            let body = doc.body().to_string();
            (doc.frontmatter, title, body)
        }
        "text" => {
            let body = std::fs::read_to_string(path)
                .with_context(|| format!("read {}", path.display()))?;
            let title = heading_title(&body).unwrap_or_else(|| stem.clone());
            (Frontmatter::none(), title, body)
        }
        "epub" => {
            let book = crate::epub::read_epub(path)?;
            let body = book.to_markdown();
            let title = book.title.filter(|t| !t.is_empty()).unwrap_or(stem);
            (Frontmatter::none(), title, body)
        }
        other => anyhow::bail!("unsupported source type {other}"),
    };

    // Provenance (A8): the copy declares where it came from. Existing
    // frontmatter keys survive; `id` and `title` are only filled in when
    // the source had none.
    let id = fm
        .get_str("id")
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| new_id("ref").to_string());
    let source_path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if fm.is_some() {
        if fm.get_str("title").filter(|t| !t.is_empty()).is_none() {
            fm.set_str("title", &title)?;
        }
        fm.set_str("id", &id)?;
        fm.set_str("source_type", source_type)?;
        fm.set_str("source_path", &source_path.to_string_lossy())?;
        fm.set_str("imported", &today_utc())?;
    } else {
        fm = Frontmatter::create(&[
            ("id", &id),
            ("title", &title),
            ("source_type", source_type),
            ("source_path", &source_path.to_string_lossy()),
            ("imported", &today_utc()),
        ])?;
    }

    let dir = project_root.join("library");
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    let base = slugify(&title);
    let mut slug = base.clone();
    let mut n = 2;
    while dir.join(format!("{slug}.md")).exists() {
        slug = format!("{base}-{n}");
        n += 1;
    }
    let rel = PathBuf::from("library").join(format!("{slug}.md"));
    let mut out = fm.raw().unwrap_or_default().to_string();
    out.push_str(&body);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    write_new(&project_root.join(&rel), out.as_bytes())
        .with_context(|| format!("write {}", rel.display()))?;
    tracing::info!(rel = %rel.display(), source_type, "reference imported");
    Ok(Imported {
        rel,
        title,
        source_type,
        id,
    })
}

/// The document's own top-level heading, if it has one — a better title
/// than the filename stem for hand-authored references.
fn heading_title(body: &str) -> Option<String> {
    body.lines()
        .find_map(|line| line.strip_prefix("# "))
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
}

fn source_type(path: &Path) -> Result<&'static str> {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "md" | "markdown" => Ok("markdown"),
        "txt" | "text" => Ok("text"),
        "epub" => Ok("epub"),
        _ => Err(anyhow!(
            "unsupported file \".{ext}\" — import accepts Markdown (.md), plain text \
             (.txt), and EPUB (.epub)"
        )),
    }
}

/// Today's UTC date (`YYYY-MM-DD`) — provenance, not a clock feature.
fn today_utc() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    utc_date((secs / 86_400) as i64)
}

/// Days-since-epoch → civil date (Howard Hinnant's `civil_from_days`).
fn utc_date(days: i64) -> String {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_date_matches_known_epochs() {
        assert_eq!(utc_date(0), "1970-01-01");
        assert_eq!(utc_date(19_723), "2024-01-01");
        assert_eq!(utc_date(20_753), "2026-10-27");
    }

    #[test]
    fn the_title_prefers_the_documents_own_heading() {
        assert_eq!(
            heading_title("# The Harbor Code\n\nProse.\n"),
            Some("The Harbor Code".into())
        );
        // Not a heading: two hashes, or text without one.
        assert_eq!(heading_title("## Sub\n"), None);
        assert_eq!(heading_title("plain prose\n"), None);
    }

    #[test]
    fn unsupported_sources_say_what_is_accepted() {
        let tmp = tempfile::TempDir::new().unwrap();
        let pdf = tmp.path().join("book.pdf");
        std::fs::write(&pdf, b"%PDF").unwrap();
        let err = import_file(tmp.path(), &pdf).unwrap_err().to_string();
        assert!(err.contains("Markdown"), "{err}");
        assert!(err.contains("EPUB"), "{err}");
    }

    #[test]
    fn markdown_import_gains_provenance_and_keeps_existing_keys() {
        let tmp = tempfile::TempDir::new().unwrap();
        let src = tmp.path().join("notes.md");
        std::fs::write(
            &src,
            "---\nseries: Field Notes\n---\n\nThe tide table, transcribed.\n",
        )
        .unwrap();
        let imported = import_file(tmp.path(), &src).unwrap();
        assert_eq!(imported.source_type, "markdown");
        let copied = std::fs::read_to_string(tmp.path().join(&imported.rel)).unwrap();
        assert!(
            copied.contains("series: Field Notes"),
            "unknown keys survive"
        );
        assert!(copied.contains("source_type: markdown"), "{copied}");
        assert!(copied.contains("source_path:"), "{copied}");
        assert!(copied.contains("The tide table, transcribed."), "{copied}");
        let doc = parse(&copied);
        assert_eq!(
            doc.frontmatter.get_str("id").as_deref(),
            Some(imported.id.as_str())
        );
    }

    #[test]
    fn text_import_becomes_markdown_with_provenance() {
        let tmp = tempfile::TempDir::new().unwrap();
        let src = tmp.path().join("almanac.txt");
        std::fs::write(&src, "Fog since Tuesday.\n").unwrap();
        let imported = import_file(tmp.path(), &src).unwrap();
        assert_eq!(imported.rel, PathBuf::from("library/almanac.md"));
        let copied = std::fs::read_to_string(tmp.path().join(&imported.rel)).unwrap();
        assert!(copied.contains("title: almanac"), "{copied}");
        assert!(copied.contains("source_type: text"), "{copied}");
        assert!(copied.contains("Fog since Tuesday."), "{copied}");
    }

    #[test]
    fn colliding_titles_get_distinct_files() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("one")).unwrap();
        std::fs::create_dir_all(tmp.path().join("two")).unwrap();
        for dir in ["one", "two"] {
            std::fs::write(
                tmp.path().join(dir).join("notes.md"),
                "---\ntitle: Same Title Here\n---\nBody.\n",
            )
            .unwrap();
        }
        let first = import_file(tmp.path(), &tmp.path().join("one/notes.md")).unwrap();
        let second = import_file(tmp.path(), &tmp.path().join("two/notes.md")).unwrap();
        assert_eq!(first.rel, PathBuf::from("library/same-title-here.md"));
        assert_eq!(second.rel, PathBuf::from("library/same-title-here-2.md"));
    }
}
