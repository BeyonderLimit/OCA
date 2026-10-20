//! Story continuity engine (Phase 12): deterministic contradiction and
//! drift detection over the Phase 2 substrate — states, aliases,
//! relationships, timeline events, world documents, and wiki links.
//!
//! Reports only: this module never edits a project file. Triage is always
//! an explicit user action — [`set_ignored`] records a decision in
//! `.oca/state/` (precious, rule 2), [`note_for`] writes an author-chosen
//! note through the Core. Severity is honest: `Error` is a broken
//! reference or definite contradiction, `Warning` an ambiguity the author
//! should look at, `Info` a nudge.
//!
//! The engine knows nothing about models (rule 3): retrieval-based
//! corroboration is a *frontend* concern — each issue carries a
//! deterministic [`Issue::query`] a frontend may feed to the hybrid
//! search, and whatever comes back is untrusted context (A8).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use oca_core::{Command, Core};
use oca_markdown::with_body;
use oca_model::DocumentKind;
use oca_storage::atomic::atomic_write;

use crate::{Chapter, Story, position_of};

/// Where triage decisions live (rule 2: precious state, never cache).
pub const IGNORED_REL: &str = ".oca/state/continuity/ignored.txt";
/// The note document `note_for` writes into.
pub const NOTE_REL: &str = "notes/continuity.md";

/// How sure the engine is. Ordering matters: frontends sort by it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    /// A nudge — worth knowing, rarely worth stopping for.
    Info,
    /// Ambiguity or drift the author should look at.
    Warning,
    /// A broken reference or definite contradiction.
    Error,
}

impl Severity {
    /// Frontend-facing label (`HIGH`/`WARN`/`INFO`).
    pub fn label(self) -> &'static str {
        match self {
            Severity::Error => "HIGH",
            Severity::Warning => "WARN",
            Severity::Info => "INFO",
        }
    }
}

/// One document an issue points at ("View references").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IssueRef {
    pub rel: PathBuf,
    pub title: String,
}

/// One continuity issue. `id` is stable across runs for the same project
/// content, so ignore decisions survive re-checks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Issue {
    /// Stable identity: `<check>|<subject>`.
    pub id: String,
    /// Stable machine id of the check (`character/unknown-state-chapter`).
    pub check: &'static str,
    /// Domain: `character`, `location`, `timeline`, `relationship`,
    /// `object`, or `world`.
    pub domain: &'static str,
    pub severity: Severity,
    /// Human sentence — what's wrong and where.
    pub message: String,
    /// Deterministic corroborator query (a name, alias, or location —
    /// never prose), for frontends that want supporting passages.
    pub query: String,
    pub refs: Vec<IssueRef>,
}

impl Issue {
    fn new(
        check: &'static str,
        domain: &'static str,
        severity: Severity,
        subject: &str,
        message: String,
        query: impl Into<String>,
        refs: Vec<IssueRef>,
    ) -> Self {
        Issue {
            id: format!("{check}|{subject}"),
            check,
            domain,
            severity,
            message,
            query: query.into(),
            refs,
        }
    }
}

/// Run every continuity check. Pure read: files never change (reports
/// only). Issues come back in a deterministic order.
pub fn check(core: &Core) -> Result<Vec<Issue>> {
    let story = Story::new(core);
    let chs = story.chapters()?;
    let chars = story.characters()?;
    // A project with no timeline yet simply has no events to check — the
    // `events()` query refuses ("record events there…"), but continuity is
    // a diagnosis pass and must run on every project the author opens.
    let has_timeline = core
        .project()
        .map(|p| {
            p.documents()
                .iter()
                .any(|d| d.rel == Path::new(crate::TIMELINE_REL))
        })
        .unwrap_or(false);
    let events = if has_timeline {
        story.events().unwrap_or_default()
    } else {
        Vec::new()
    };
    let mut out = Vec::new();
    check_states(&chs, &chars, &mut out);
    check_alias_collisions(&chars, &mut out);
    check_locations(&chars, core, &mut out);
    check_timeline(&chs, &events, core, &mut out);
    check_relationships(&chars, &mut out);
    check_wiki_links(core, &mut out)?;
    out.sort_by(|a, b| {
        b.severity
            .cmp(&a.severity)
            .then_with(|| a.check.cmp(b.check))
            .then_with(|| a.id.cmp(&b.id))
    });
    Ok(out)
}

/// Canonical one-line-per-issue rendering: `SEVERITY\t<id>\t<refs>\t<message>`
/// (refs comma-joined, message last so it may contain anything but tabs).
/// This is the machine form evals grade; frontends print their own layout.
pub fn render(issues: &[Issue]) -> String {
    let mut out = String::new();
    for i in issues {
        out.push_str(i.severity.label());
        out.push('\t');
        out.push_str(&i.id);
        out.push('\t');
        out.push_str(
            &i.refs
                .iter()
                .map(|r| r.rel.display().to_string())
                .collect::<Vec<_>>()
                .join(","),
        );
        out.push('\t');
        out.push_str(&i.message);
        out.push('\n');
    }
    out
}

// ---- character domain ------------------------------------------------------

fn check_states(chs: &[Chapter], chars: &[crate::Character], out: &mut Vec<Issue>) {
    for c in chars {
        let mut seen: BTreeMap<(String, String), usize> = BTreeMap::new();
        for entry in &c.states {
            // A state keyed at a chapter that doesn't exist is silently
            // dropped by carry-forward — the author should know.
            if position_of(chs, &entry.at).is_none() {
                out.push(Issue::new(
                    "character/unknown-state-chapter",
                    "character",
                    Severity::Error,
                    &format!("{}|{}", c.id, entry.at),
                    format!(
                        "{name} has a state at chapter \"{at}\" — no such chapter, so the \
                         entry never applies (did the chapter get deleted or renamed?)",
                        name = c.name,
                        at = entry.at
                    ),
                    c.name.clone(),
                    vec![ref_of(&c.rel, &c.title)],
                ));
            }
            for field in entry.fields.keys() {
                *seen.entry((entry.at.clone(), field.clone())).or_insert(0) += 1;
            }
        }
        for ((at, field), n) in seen {
            if n > 1 {
                out.push(Issue::new(
                    "character/duplicate-state-field",
                    "character",
                    Severity::Warning,
                    &format!("{}|{at}|{field}", c.id),
                    format!(
                        "{name} sets \"{field}\" twice at chapter \"{at}\" — the later entry \
                         silently wins",
                        name = c.name
                    ),
                    c.name.clone(),
                    vec![ref_of(&c.rel, &c.title)],
                ));
            }
        }
    }
}

fn check_alias_collisions(chars: &[crate::Character], out: &mut Vec<Issue>) {
    // name + aliases of every character, keyed by normalized form.
    let mut claimants: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut verbatim: BTreeMap<String, String> = BTreeMap::new();
    for c in chars {
        for name in std::iter::once(&c.name).chain(c.aliases.iter()) {
            if name.trim().is_empty() {
                continue;
            }
            let key = normalize(name);
            claimants
                .entry(key.clone())
                .or_default()
                .insert(c.name.clone());
            verbatim.entry(key).or_insert_with(|| name.clone());
        }
    }
    let ref_by_name = |name: &str| -> Vec<IssueRef> {
        chars
            .iter()
            .filter(|c| c.name == name)
            .map(|c| ref_of(&c.rel, &c.name))
            .collect()
    };
    for (key, owners) in claimants {
        if owners.len() > 1 {
            let shown = verbatim.get(&key).cloned().unwrap_or_default();
            let mut refs: Vec<IssueRef> = owners.iter().flat_map(|n| ref_by_name(n)).collect();
            refs.dedup();
            out.push(Issue::new(
                "character/alias-collision",
                "character",
                Severity::Warning,
                &key,
                format!(
                    "the name \"{shown}\" is claimed by {} — mentions are ambiguous",
                    owners.into_iter().collect::<Vec<_>>().join(" and ")
                ),
                shown.clone(),
                refs,
            ));
        }
    }
}

// ---- location domain -------------------------------------------------------

fn check_locations(chars: &[crate::Character], core: &Core, out: &mut Vec<Issue>) {
    // verbatim location value → who uses it.
    let mut usages: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for c in chars {
        for entry in &c.states {
            if let Some(loc) = entry.fields.get("location")
                && !loc.trim().is_empty()
            {
                usages
                    .entry(loc.clone())
                    .or_default()
                    .insert(c.name.clone());
            }
        }
    }
    // One place, written N ways: drift.
    let mut by_key: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for loc in usages.keys() {
        by_key.entry(normalize(loc)).or_default().push(loc.clone());
    }
    // The character files behind a set of user names — every location
    // issue points at the states that caused it.
    let refs_for = |names: &BTreeSet<String>| -> Vec<IssueRef> {
        chars
            .iter()
            .filter(|c| names.contains(&c.name))
            .map(|c| ref_of(&c.rel, &c.name))
            .collect()
    };
    for (key, mut variants) in by_key {
        if variants.len() > 1 {
            variants.sort();
            let users: BTreeSet<String> = variants
                .iter()
                .flat_map(|v| usages.get(v).into_iter().flatten().cloned())
                .collect();
            out.push(Issue::new(
                "location/spelling-drift",
                "location",
                Severity::Warning,
                &key,
                format!(
                    "a location is written {} ways: {} — state carry-forward treats \
                     them as different places",
                    variants.len(),
                    variants
                        .iter()
                        .map(|v| format!("\"{v}\""))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                variants[0].clone(),
                refs_for(&users),
            ));
        }
    }
    // Locations that no world document defines (nudge only).
    let defined: BTreeSet<String> = world_names(core)
        .unwrap_or_default()
        .iter()
        .map(|n| normalize(n))
        .collect();
    let mut undefined: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (loc, users) in &usages {
        if !defined.contains(&normalize(loc)) {
            undefined
                .entry(normalize(loc))
                .or_default()
                .extend(users.clone());
        }
    }
    for (key, users) in undefined {
        // A spelling-drift issue already covers the variants; don't double
        // report every variant of the same normalized name.
        let shown = usages
            .keys()
            .find(|l| normalize(l) == key)
            .cloned()
            .unwrap_or(key.clone());
        let refs = refs_for(&users);
        out.push(Issue::new(
            "world/location-undefined",
            "world",
            Severity::Info,
            &key,
            format!(
                "the location \"{shown}\" (used by {}) has no world/ document — define \
                 it there so its rules travel with the story",
                users.into_iter().collect::<Vec<_>>().join(", ")
            ),
            shown.clone(),
            refs,
        ));
    }
}

// ---- timeline domain -------------------------------------------------------

fn check_timeline(chs: &[Chapter], events: &[crate::Event], core: &Core, out: &mut Vec<Issue>) {
    let mut ids: BTreeMap<&str, usize> = BTreeMap::new();
    for e in events {
        *ids.entry(e.id.as_str()).or_insert(0) += 1;
        if let Some(at) = &e.chapter
            && position_of(chs, at).is_none()
        {
            out.push(Issue::new(
                "timeline/event-unknown-chapter",
                "timeline",
                Severity::Error,
                &e.id,
                format!(
                    "event \"{title}\" is shown in chapter \"{at}\" — no such chapter, so \
                     it falls out of narrative-order queries",
                    title = e.title
                ),
                e.title.clone(),
                vec![ref_of(Path::new(crate::TIMELINE_REL), "Timeline")],
            ));
        }
        let st = e.story_time.trim();
        if !st.is_empty()
            && (st.contains(char::is_whitespace) || !st.starts_with(|c: char| c.is_ascii_digit()))
        {
            out.push(Issue::new(
                "timeline/unsortable-story-time",
                "timeline",
                Severity::Warning,
                &e.id,
                format!(
                    "event \"{title}\" has story_time \"{st}\" — story times compare as \
                     strings, so this sorts unpredictably (use 1888-03-18 or another \
                     zero-padded scheme)",
                    title = e.title
                ),
                e.title.clone(),
                vec![ref_of(Path::new(crate::TIMELINE_REL), "Timeline")],
            ));
        }
    }
    for (id, n) in ids {
        if n > 1 {
            out.push(Issue::new(
                "timeline/duplicate-event-id",
                "timeline",
                Severity::Error,
                id,
                format!("event id \"{id}\" appears {n} times — ids must be unique"),
                id.to_string(),
                vec![ref_of(Path::new(crate::TIMELINE_REL), "Timeline")],
            ));
        }
    }
    // Mixed story_time coverage: some chapters dated, others not.
    let mut dated = 0usize;
    let mut undated: Vec<&Chapter> = Vec::new();
    for ch in chs {
        let has = core
            .read_document(&ch.rel)
            .map(|d| d.frontmatter.get_str("story_time").is_some())
            .unwrap_or(false);
        if has {
            dated += 1;
        } else {
            undated.push(ch);
        }
    }
    if dated > 0 && !undated.is_empty() {
        out.push(Issue::new(
            "timeline/chapter-missing-story-time",
            "timeline",
            Severity::Info,
            "coverage",
            format!(
                "{dated} chapter(s) carry story_time, {} do not ({}) — story-sense \
                 queries silently skip undated chapters",
                undated.len(),
                undated
                    .iter()
                    .take(3)
                    .map(|c| c.title.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            String::new(),
            undated.iter().map(|c| ref_of(&c.rel, &c.title)).collect(),
        ));
    }
}

// ---- relationship domain ---------------------------------------------------

fn check_relationships(chars: &[crate::Character], out: &mut Vec<Issue>) {
    let resolve = |key: &str| -> Option<String> {
        let lower = key.to_lowercase();
        chars
            .iter()
            .find(|c| {
                c.id == key
                    || c.name.to_lowercase() == lower
                    || c.aliases.iter().any(|a| a.to_lowercase() == lower)
            })
            .map(|c| c.name.clone())
    };
    for c in chars {
        for rel in &c.relationships {
            if resolve(&rel.target).is_none() {
                out.push(Issue::new(
                    "relationship/target-missing",
                    "relationship",
                    Severity::Error,
                    &format!("{}|{}", c.id, rel.target),
                    format!(
                        "{name} has a \"{kind}\" relationship with \"{target}\" — no such \
                         character (ids and names both work)",
                        name = c.name,
                        kind = rel.kind,
                        target = rel.target
                    ),
                    rel.target.clone(),
                    vec![ref_of(&c.rel, &c.title)],
                ));
            }
        }
    }
    // Both sides declared, kinds disagree: worth a look (often intentional —
    // dramatic irony — hence Info, never a warning).
    for a in chars {
        for ra in &a.relationships {
            let Some(b) = chars
                .iter()
                .find(|c| c.id == ra.target || c.name == ra.target)
            else {
                continue;
            };
            if b.id == a.id {
                continue;
            }
            // One issue per pair, canonically ordered — a mismatch is one
            // fact, not two.
            if a.id >= b.id {
                continue;
            }
            if let Some(rb) = b
                .relationships
                .iter()
                .find(|r| r.target == a.id || r.target == a.name)
                && rb.kind != ra.kind
            {
                out.push(Issue::new(
                    "relationship/reciprocal-mismatch",
                    "relationship",
                    Severity::Info,
                    &format!("{}|{}", a.id, b.id),
                    format!(
                        "{an} -> {bn} is \"{ak}\" but {bn} -> {an} is \"{bk}\"",
                        an = a.name,
                        bn = b.name,
                        ak = ra.kind,
                        bk = rb.kind
                    ),
                    a.name.clone(),
                    vec![ref_of(&a.rel, &a.title), ref_of(&b.rel, &b.title)],
                ));
            }
        }
    }
}

// ---- object domain (wiki links by id, A2) ----------------------------------

fn check_wiki_links(core: &Core, out: &mut Vec<Issue>) -> Result<()> {
    let Some(project) = core.project() else {
        return Ok(());
    };
    let known: BTreeSet<&str> = project
        .documents()
        .iter()
        .filter_map(|d| d.id.as_deref())
        .collect();
    for entry in project.documents() {
        let Ok(doc) = core.read_document(&entry.rel) else {
            continue;
        };
        for link in &doc.links {
            if !known.contains(link.target.as_str()) {
                out.push(Issue::new(
                    "object/broken-wiki-link",
                    "object",
                    Severity::Error,
                    &format!("{}|{}", entry.rel.display(), link.target),
                    format!(
                        "[[{target}]] in {doc} points at no document — links use immutable \
                         frontmatter ids, so a renamed target keeps its id",
                        target = link.target,
                        doc = entry.rel.display()
                    ),
                    link.target.clone(),
                    vec![ref_of(&entry.rel, &entry.title)],
                ));
            }
        }
    }
    Ok(())
}

// ---- helpers ---------------------------------------------------------------

fn ref_of(rel: &Path, title: &str) -> IssueRef {
    IssueRef {
        rel: rel.to_path_buf(),
        title: title.to_string(),
    }
}

/// Normalized comparison form: lowercase, "the " stripped, alphanumerics
/// only — so "The Harbor" == "the-harbor" == "harbor" for drift/definition
/// matching (display always keeps the verbatim spelling).
fn normalize(s: &str) -> String {
    let lower = s.trim().to_lowercase();
    let lower = lower.strip_prefix("the ").unwrap_or(&lower).to_string();
    lower.chars().filter(|c| c.is_alphanumeric()).collect()
}

/// Titles + ids of every `world/` document.
fn world_names(core: &Core) -> Result<Vec<String>> {
    let Some(project) = core.project() else {
        return Ok(Vec::new());
    };
    Ok(project
        .documents()
        .iter()
        .filter(|d| d.group() == "world")
        .flat_map(|d| {
            let mut names = vec![d.title.clone()];
            if let Some(stem) = d.rel.file_stem().and_then(|s| s.to_str()) {
                names.push(stem.to_string());
            }
            names.extend(d.id.clone());
            names
        })
        .collect())
}

// ---- triage ----------------------------------------------------------------

/// Every ignored issue id (`.oca/state/continuity/ignored.txt`).
pub fn ignored_ids(core: &Core) -> Result<BTreeSet<String>> {
    let Some(project) = core.project() else {
        return Ok(BTreeSet::new());
    };
    read_ignored(project.root())
}

fn read_ignored(root: &Path) -> Result<BTreeSet<String>> {
    let path = root.join(IGNORED_REL);
    if !path.is_file() {
        return Ok(BTreeSet::new());
    }
    let raw = std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    Ok(raw
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_string)
        .collect())
}

/// Record or lift an ignore decision (precious state, rule 2). The issue's
/// own file is never touched — decisions live only in `.oca/state/`.
pub fn set_ignored(core: &Core, id: &str, ignored: bool) -> Result<()> {
    let project = core
        .project()
        .context("no project is open — triage needs an open project")?;
    let mut set = read_ignored(project.root())?;
    if ignored {
        set.insert(id.to_string());
    } else {
        set.remove(id);
    }
    let mut body = String::from("# ignored continuity issue ids — one per line\n");
    for id in &set {
        body.push_str(id);
        body.push('\n');
    }
    atomic_write(&project.root().join(IGNORED_REL), body.as_bytes())
        .with_context(|| format!("write {IGNORED_REL}"))
}

/// Append one issue to the continuity note (creates it on first use),
/// through the Core so snapshots, tags, and the index all see it. This is
/// an explicit author action — the engine never writes it on its own.
pub fn note_for(core: &mut Core, issue: &Issue) -> Result<PathBuf> {
    let exists = core
        .project()
        .map(|p| p.root().join(NOTE_REL).is_file())
        .unwrap_or(false);
    if !exists {
        core.execute(Command::CreateDocument {
            kind: DocumentKind::Note,
            title: "Continuity".into(),
        })?;
    }
    let doc = core.read_document(Path::new(NOTE_REL))?;
    let mut body = doc.body().to_string();
    if !body.ends_with('\n') && !body.is_empty() {
        body.push('\n');
    }
    body.push_str(&format!(
        "\n## {severity} — {check}\n\n{message}\n",
        severity = issue.severity.label(),
        check = issue.check,
        message = issue.message
    ));
    let raw = with_body(&doc, &body);
    core.execute(Command::SaveDocument {
        doc: PathBuf::from(NOTE_REL),
        raw,
    })?;
    Ok(PathBuf::from(NOTE_REL))
}

#[cfg(test)]
mod tests {
    use super::*;
    use oca_model::ProjectKind;

    fn save(core: &mut Core, rel: &str, raw: &str) {
        core.execute(Command::SaveDocument {
            doc: PathBuf::from(rel),
            raw: raw.into(),
        })
        .unwrap();
    }

    /// A small project with one seeded issue per check — the same shape the
    /// `oca-eval` grader asserts against, but built fresh per test.
    fn seeded() -> (tempfile::TempDir, Core) {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut core = Core::new();
        core.execute(Command::CreateProject {
            root: tmp.path().to_path_buf(),
            title: "Continuity".into(),
            kind: ProjectKind::Novel,
        })
        .unwrap();
        // Chapter ids pinned so `at:` / `chapter:` keys are predictable;
        // ch_beta alone carries story_time (mixed coverage → info).
        save(
            &mut core,
            "manuscript/001-the-arrival.md",
            "---\nid: ch_alpha\ntitle: The Arrival\n---\n\n# The Arrival\n\n\
             Elena read the letter. A note about [[ghost-doc]] too.\n",
        );
        save(
            &mut core,
            "manuscript/002-the-trial.md",
            "---\nid: ch_beta\ntitle: The Trial\nstory_time: '1888-02-02'\n---\n\n\
             # The Trial\n\nProse.\n",
        );
        save(
            &mut core,
            "characters/elena.md",
            "---\nid: elena\ntitle: Elena\nname: Elena\naliases: [Ellie, \"the girl\"]\n\
             relationships:\n  barnaby:\n    type: trust\n\
             states:\n  - at: ch_alpha\n    location: blackwood\n    emotional: waiting\n  \
             - at: ch_gone\n    location: nowhere\n  \
             - at: ch_alpha\n    emotional: restless\n  \
             - at: ch_beta\n    location: Blackwood\n---\n\n# Elena\n",
        );
        save(
            &mut core,
            "characters/barnaby.md",
            "---\nid: barnaby\ntitle: Barnaby\nname: Barnaby\naliases: [\"the girl\"]\n\
             relationships:\n  elena:\n    type: distrust\n  ghost:\n    type: fears\n---\n\n\
             # Barnaby\n",
        );
        save(
            &mut core,
            "world/blackwood.md",
            "---\nid: world_blackwood\ntitle: Blackwood\n---\n\n# Blackwood\n\n\
             A forest with rules.\n",
        );
        save(
            &mut core,
            "outline/timeline.md",
            "---\nevents:\n  - id: ev_ok\n    title: The arrival\n    story_time: '1888-01-01'\n    \
             chapter: ch_alpha\n  \
             - id: ev_lost\n    title: The vanished scene\n    story_time: '1888-01-05'\n    \
             chapter: ch_99\n  \
             - id: ev_dup\n    title: First telling\n    story_time: '1888-01-06'\n  \
             - id: ev_dup\n    title: Second telling\n    story_time: '1888-01-07'\n  \
             - id: ev_march\n    title: The marching\n    story_time: March 1888\n    \
             chapter: ch_alpha\n---\n\n# Timeline\n",
        );
        (tmp, core)
    }

    fn find<'i>(issues: &'i [Issue], id: &str) -> &'i Issue {
        issues.iter().find(|i| i.id == id).unwrap_or_else(|| {
            let known: Vec<&str> = issues.iter().map(|i| i.id.as_str()).collect();
            panic!("missing issue {id:?}; got {known:?}")
        })
    }

    #[test]
    fn seeded_contradictions_surface_with_severity_and_refs() {
        let (_tmp, core) = seeded();
        let issues = check(&core).unwrap();

        // Errors: broken references and definite contradictions.
        for id in [
            "character/unknown-state-chapter|elena|ch_gone",
            "timeline/event-unknown-chapter|ev_lost",
            "timeline/duplicate-event-id|ev_dup",
            "relationship/target-missing|barnaby|ghost",
            "object/broken-wiki-link|manuscript/001-the-arrival.md|ghost-doc",
        ] {
            assert_eq!(find(&issues, id).severity, Severity::Error, "{id}");
        }
        // Warnings: ambiguity and drift the author should look at.
        for id in [
            "character/duplicate-state-field|elena|ch_alpha|emotional",
            "character/alias-collision|girl",
            "location/spelling-drift|blackwood",
            "timeline/unsortable-story-time|ev_march",
        ] {
            assert_eq!(find(&issues, id).severity, Severity::Warning, "{id}");
        }
        // Info: honest nudges only.
        assert_eq!(
            find(&issues, "relationship/reciprocal-mismatch|barnaby|elena").severity,
            Severity::Info
        );
        assert_eq!(
            issues
                .iter()
                .filter(|i| i.check == "relationship/reciprocal-mismatch")
                .count(),
            1,
            "one issue per pair, not one per direction"
        );
        assert_eq!(
            find(&issues, "timeline/chapter-missing-story-time|coverage").severity,
            Severity::Info
        );
        assert!(
            issues
                .iter()
                .any(|i| i.id == "world/location-undefined|nowhere"),
            "an undefined location is an info nudge: {:#?}",
            issues.iter().map(|i| &i.id).collect::<Vec<_>>()
        );
        assert!(
            !issues
                .iter()
                .any(|i| { i.check == "world/location-undefined" && i.id.contains("blackwood") }),
            "Blackwood is defined in world/ — no false nudge"
        );

        // References: every error points at a document you can open.
        for i in issues.iter().filter(|i| i.severity == Severity::Error) {
            assert!(!i.refs.is_empty(), "error without refs: {i:?}");
            assert!(!i.refs[0].title.is_empty(), "ref without a title: {i:?}");
        }
        // A state the engine cannot place names its character.
        let unknown = find(&issues, "character/unknown-state-chapter|elena|ch_gone");
        assert_eq!(unknown.refs[0].rel, PathBuf::from("characters/elena.md"));
        assert_eq!(unknown.query, "Elena");
        // Deterministic: the same run twice, byte for byte.
        assert_eq!(issues, check(&core).unwrap());
        // Reports only: nothing on disk changed.
        let before = std::fs::read_to_string(_tmp.path().join("characters/elena.md")).unwrap();
        assert!(before.contains("ch_gone"), "files never touched");
    }

    #[test]
    fn triage_ignores_and_notes_are_explicit_actions() {
        let (tmp, mut core) = seeded();
        let issues = check(&core).unwrap();
        let target = issues
            .iter()
            .find(|i| i.check == "character/unknown-state-chapter")
            .cloned()
            .unwrap();

        // Ignore lives in .oca/state (precious, rule 2) — never in files.
        set_ignored(&core, &target.id, true).unwrap();
        assert!(ignored_ids(&core).unwrap().contains(&target.id));
        let elena = std::fs::read_to_string(tmp.path().join("characters/elena.md")).unwrap();
        assert!(elena.contains("ch_gone"), "the character file is untouched");
        set_ignored(&core, &target.id, false).unwrap();
        assert!(ignored_ids(&core).unwrap().is_empty());

        // Creating a note is explicit, appends on repeat, and goes through
        // the Core (so it shows up in the tree and the index).
        let rel = note_for(&mut core, &target).unwrap();
        assert_eq!(rel, PathBuf::from(NOTE_REL));
        let note = std::fs::read_to_string(tmp.path().join(NOTE_REL)).unwrap();
        assert!(note.contains("HIGH"), "severity in the note: {note}");
        assert!(note.contains("ch_gone"), "message in the note: {note}");
        note_for(&mut core, &target).unwrap();
        let note2 = std::fs::read_to_string(tmp.path().join(NOTE_REL)).unwrap();
        assert_eq!(
            note2.matches("ch_gone").count(),
            note.matches("ch_gone").count() + 1,
            "second note appends, never rewrites"
        );
    }
}
