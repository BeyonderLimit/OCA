//! OCA Core API — the single choke point between frontends and the project
//! (A5 commands + events; AGENTS Rule 8: TUI/CLI are clients of *this*).
//!
//! Every mutation flows through [`Core::execute`]; events fan out to the
//! caller and to subscribers. Read-only projects are rejected here and in
//! storage, so no frontend can bypass the gate.

use std::path::{Path, PathBuf};

use anyhow::Result;
use oca_model::{Document, DocumentKind, ProjectKind, ProjectMeta};
use oca_storage::Project;
use oca_versioning::SnapshotStore;

/// Re-export so frontends can hold search/link results without a direct
/// storage dependency for the types flowing through Core.
pub use oca_storage::db::{Hit, LinkEnd, LinkReport, ReindexStats, TagCount, Totals};
/// Re-export so higher layers (story/music engines) can read the document
/// index through Core without depending on storage directly.
pub use oca_storage::index::DocEntry;
/// Re-export so frontends don't need a direct storage dependency for the
/// recovery type flowing through Core.
pub use oca_storage::journal::RecoveryEntry;
/// Snapshot types flowing through Core — frontends stay dependency-free.
pub use oca_versioning::{
    ChangeKind, DiffReport, FileDiff, RestoreReport, SnapshotInfo, format_time,
};

/// Mutations, expressible as data (later: serializable to a GUI wire format).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    CreateProject {
        root: PathBuf,
        title: String,
        kind: ProjectKind,
    },
    OpenProject {
        root: PathBuf,
    },
    CloseProject,
    CreateDocument {
        kind: DocumentKind,
        title: String,
    },
    SaveDocument {
        doc: PathBuf,
        raw: String,
    },
    AppendParagraph {
        doc: PathBuf,
        text: String,
    },
    /// Patch one frontmatter key. A value containing newlines is a
    /// structured YAML value (sequence/mapping fragment, e.g. a character's
    /// `states:` block); a single line is a scalar.
    SetFrontmatter {
        doc: PathBuf,
        key: String,
        value: String,
    },
    WriteRecovery {
        doc: PathBuf,
        content: String,
    },
    ClearRecovery {
        doc: PathBuf,
    },
    /// Save the whole project state as a restore point. History is
    /// append-only (Snapshots vocabulary, rule 7); audio takes are tracked
    /// by checksum, never duplicated.
    SnapshotCreate {
        label: Option<String>,
    },
    /// Return the project to snapshot `n`. The current state is saved
    /// first — restoring is never itself a way to lose work.
    SnapshotRestore {
        n: usize,
    },
}

/// Everything observable that happened. Frontends render these; tests assert
/// on them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    ProjectCreated {
        root: PathBuf,
        project_id: String,
    },
    ProjectOpened {
        root: PathBuf,
        title: String,
        read_only: bool,
        reason: Option<String>,
    },
    ProjectClosed,
    DocumentCreated {
        rel: PathBuf,
        title: String,
    },
    DocumentSaved {
        rel: PathBuf,
    },
    RecoveryJournaled {
        rel: PathBuf,
    },
    RecoveryCleared {
        rel: PathBuf,
    },
    SnapshotCreated {
        n: usize,
        label: String,
        files: usize,
    },
    SnapshotRestored {
        n: usize,
        restored: usize,
        removed: usize,
        unchanged: usize,
        audio_differing: Vec<PathBuf>,
    },
}

/// Snapshot count for a project directory without opening the project
/// (`doctor` uses this; listing never touches the working files).
pub fn snapshot_summary(root: &Path) -> Result<(usize, Option<String>)> {
    let list = oca_versioning::GitStore::new().list(root)?;
    let newest = list.last().map(|s| s.display_label());
    Ok((list.len(), newest))
}

type Listener = Box<dyn FnMut(&Event)>;

#[derive(Default)]
pub struct Core {
    project: Option<Project>,
    listeners: Vec<Listener>,
    /// The snapshot engine (swappable behind `SnapshotStore`).
    store: oca_versioning::GitStore,
}

impl Core {
    pub fn new() -> Self {
        Self::default()
    }

    /// Subscribe to every event `execute` emits (GUI hooks).
    pub fn subscribe(&mut self, listener: Listener) {
        self.listeners.push(listener);
    }

    pub fn project(&self) -> Option<&Project> {
        self.project.as_ref()
    }

    fn project_mut(&mut self) -> Result<&mut Project> {
        self.project
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("no project is open"))
    }

    /// Run a command; returns the events it caused (also delivered to
    /// listeners).
    pub fn execute(&mut self, cmd: Command) -> Result<Vec<Event>> {
        let events = match cmd {
            Command::CreateProject { root, title, kind } => {
                if self.project.is_some() {
                    anyhow::bail!("close the current project first");
                }
                let project = Project::create(&root, &title, kind)?;
                let event = Event::ProjectCreated {
                    root: project.root().to_path_buf(),
                    project_id: project.meta().project_id().to_string(),
                };
                self.project = Some(project);
                vec![event]
            }
            Command::OpenProject { root } => {
                if self.project.is_some() {
                    anyhow::bail!("close the current project first");
                }
                let project = Project::open(&root)?;
                let event = Event::ProjectOpened {
                    root: project.root().to_path_buf(),
                    title: project.meta().title(),
                    read_only: project.read_only(),
                    reason: project.read_only_reason(),
                };
                self.project = Some(project);
                vec![event]
            }
            Command::CloseProject => {
                if self.project.take().is_none() {
                    anyhow::bail!("no project is open");
                }
                vec![Event::ProjectClosed]
            }
            Command::CreateDocument { kind, title } => {
                let rel = self.project_mut()?.create_document(kind, &title)?;
                vec![Event::DocumentCreated { rel, title }]
            }
            Command::SaveDocument { doc, raw } => {
                let project = self.project_mut()?;
                project.save_document(&doc, &raw)?;
                // A successful save supersedes any journaled draft.
                let had_recovery = project.recovery_for(&doc)?.is_some();
                if had_recovery {
                    project.clear_recovery(&doc)?;
                }
                let mut events = vec![Event::DocumentSaved { rel: doc.clone() }];
                if had_recovery {
                    events.push(Event::RecoveryCleared { rel: doc });
                }
                events
            }
            Command::AppendParagraph { doc, text } => {
                self.project_mut()?.append_paragraph(&doc, &text)?;
                vec![Event::DocumentSaved { rel: doc }]
            }
            Command::SetFrontmatter { doc, key, value } => {
                self.project_mut()?.set_frontmatter(&doc, &key, &value)?;
                vec![Event::DocumentSaved { rel: doc }]
            }
            Command::WriteRecovery { doc, content } => {
                self.project_mut()?.write_recovery(&doc, &content)?;
                vec![Event::RecoveryJournaled { rel: doc }]
            }
            Command::ClearRecovery { doc } => {
                self.project_mut()?.clear_recovery(&doc)?;
                vec![Event::RecoveryCleared { rel: doc }]
            }
            Command::SnapshotCreate { label } => {
                let root = self.snapshot_root("save snapshots")?;
                let info = self.store.create(&root, label.as_deref())?;
                vec![Event::SnapshotCreated {
                    n: info.n,
                    label: info.display_label(),
                    files: info.files,
                }]
            }
            Command::SnapshotRestore { n } => {
                oca_versioning::validate_n(n)?;
                let root = self.snapshot_root("restore snapshots")?;
                // Auto-snapshot first — the same hook the AI proposal
                // apply, bulk edit, and import paths will use. If the
                // current state cannot be saved, the restore is refused.
                let before = self
                    .store
                    .create(&root, Some(&format!("Before restoring snapshot #{n}")))
                    .map_err(|e| {
                        anyhow::anyhow!(
                            "cannot save the current state before restoring ({e}) — restore refused"
                        )
                    })?;
                let report = self.store.restore(&root, n)?;
                // Files changed on disk: refresh the cache index (best
                // effort — the index is derived state).
                if let Some(Err(err)) = self.project.as_mut().map(Project::reconcile) {
                    tracing::warn!(error = %err, "index refresh after restore failed");
                }
                tracing::debug!(before = before.n, "auto-snapshot before restore");
                vec![Event::SnapshotRestored {
                    n: report.n,
                    restored: report.restored,
                    removed: report.removed,
                    unchanged: report.unchanged,
                    audio_differing: report.audio_differing,
                }]
            }
        };

        for event in &events {
            for listener in &mut self.listeners {
                listener(event);
            }
        }
        Ok(events)
    }

    // ---- snapshots (Phase 6) -----------------------------------------------

    /// The project root for snapshot operations, refusing read-only
    /// sessions (while another session holds the lock, `.oca/state/` is
    /// theirs).
    fn snapshot_root(&self, what: &str) -> Result<PathBuf> {
        let project = self
            .project
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("no project is open"))?;
        if project.read_only() {
            let why = project.read_only_reason().unwrap_or_default();
            anyhow::bail!("project is read-only ({why}) — cannot {what}");
        }
        Ok(project.root().to_path_buf())
    }

    /// Every snapshot, oldest first. Empty before the first save — the
    /// store is created lazily.
    pub fn snapshots(&self) -> Result<Vec<oca_versioning::SnapshotInfo>> {
        let project = self
            .project
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("no project is open"))?;
        self.store.list(project.root())
    }

    /// Snapshot `n` compared with the working files (`against: None`) or
    /// with snapshot `m`.
    pub fn snapshot_diff(
        &self,
        n: usize,
        against: Option<usize>,
    ) -> Result<oca_versioning::DiffReport> {
        let project = self
            .project
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("no project is open"))?;
        match against {
            Some(m) => self.store.diff_pair(project.root(), n, m),
            None => self.store.diff_working(project.root(), n),
        }
    }

    // ---- read-only queries (frontends render from these) ----------------

    /// Load `rel`, splice `body` onto its (untouched) frontmatter, save.
    /// The TUI's save path — body edits never re-render frontmatter bytes.
    pub fn save_body(&mut self, rel: &Path, body: &str) -> Result<Vec<Event>> {
        let doc = self.read_document(rel)?;
        let raw = oca_markdown::with_body(&doc, body);
        self.execute(Command::SaveDocument {
            doc: rel.to_path_buf(),
            raw,
        })
    }

    pub fn is_read_only(&self) -> bool {
        self.project.as_ref().is_some_and(|p| p.read_only())
    }

    pub fn read_only_reason(&self) -> Option<String> {
        self.project.as_ref().and_then(|p| p.read_only_reason())
    }

    pub fn meta(&self) -> Option<&ProjectMeta> {
        self.project.as_ref().map(|p| p.meta())
    }

    pub fn documents(&self) -> &[DocEntry] {
        self.project.as_ref().map(|p| p.documents()).unwrap_or(&[])
    }

    pub fn groups(&self) -> Vec<(&'static str, Vec<&DocEntry>)> {
        self.project
            .as_ref()
            .map(|p| p.groups())
            .unwrap_or_default()
    }

    pub fn read_document(&self, rel: &Path) -> Result<Document> {
        let project = self
            .project
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("no project is open"))?;
        project.read_document(rel)
    }

    /// Resolve id/title → relative path (rescans on stale entries).
    pub fn resolve(&mut self, key: &str) -> Result<PathBuf> {
        self.project_mut()?.resolve(key)
    }

    pub fn recovery_for(&self, rel: &Path) -> Result<Option<oca_storage::journal::RecoveryEntry>> {
        let project = self
            .project
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("no project is open"))?;
        project.recovery_for(rel)
    }

    pub fn refresh_index(&mut self) -> Result<()> {
        self.project_mut()?.refresh_index()
    }

    // ---- cache index (Phase 5): search, tags, links, rebuild ---------------

    /// Full-text search over every indexed entity, or a `tag:ns:value` tag
    /// lookup. Fails with a pointer at `oca reindex` when no cache DB is
    /// available — never affects editing (files are canonical).
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<oca_storage::db::Hit>> {
        let project = self
            .project
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("no project is open"))?;
        project.search(query, limit)
    }

    /// Rebuild the whole cache index from the filesystem.
    pub fn reindex(&mut self) -> Result<oca_storage::db::ReindexStats> {
        self.project_mut()?.reindex_all()
    }

    /// Incremental refresh — only files that changed on disk are reparsed
    /// (the TUI calls this on an idle tick; external edits surface here).
    pub fn reconcile(&mut self) -> Result<oca_storage::db::ReindexStats> {
        self.project_mut()?.reconcile()
    }

    /// Row counts for `doctor` / `reindex` output (`None` = no project or
    /// no cache DB).
    pub fn index_totals(&self) -> Option<oca_storage::db::Totals> {
        self.project
            .as_ref()
            .and_then(|p| p.db())
            .and_then(|db| db.totals().ok())
    }

    /// Every tag in use with document counts.
    pub fn tags(&self) -> Result<Vec<oca_storage::db::TagCount>> {
        let project = self
            .project
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("no project is open"))?;
        let db = project
            .db()
            .ok_or_else(|| anyhow::anyhow!("cache index unavailable — run `oca reindex`"))?;
        db.tags()
    }

    /// Outgoing references + backlinks for a document id (ids come from
    /// `resolve` + `peek`; links never use paths).
    pub fn link_report(&self, id: &str) -> Result<oca_storage::db::LinkReport> {
        let project = self
            .project
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("no project is open"))?;
        let db = project
            .db()
            .ok_or_else(|| anyhow::anyhow!("cache index unavailable — run `oca reindex`"))?;
        db.links(id)
    }

    /// The `(rel, title, kind)` of the first index unit carrying `id` —
    /// songs, takes, and events that aren't `.md` documents and so can't be
    /// resolved through the in-memory file index.
    pub fn unit_by_ref(&self, id: &str) -> Result<Option<(PathBuf, String, String)>> {
        let project = self
            .project
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("no project is open"))?;
        let db = project
            .db()
            .ok_or_else(|| anyhow::anyhow!("cache index unavailable — run `oca reindex`"))?;
        db.unit_by_ref(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use tempfile::TempDir;

    fn opened_core(dir: &TempDir) -> Core {
        let mut core = Core::new();
        let events = core
            .execute(Command::CreateProject {
                root: dir.path().to_path_buf(),
                title: "Test Novel".into(),
                kind: ProjectKind::Novel,
            })
            .unwrap();
        assert!(matches!(events[0], Event::ProjectCreated { .. }));
        core
    }

    #[test]
    fn create_open_close_flow_with_events() {
        use std::rc::Rc;
        let tmp = TempDir::new().unwrap();
        let mut core = Core::new();

        let seen = Rc::new(RefCell::new(Vec::new()));
        let sink = Rc::clone(&seen);
        core.subscribe(Box::new(move |e: &Event| sink.borrow_mut().push(e.clone())));

        let events = core
            .execute(Command::CreateProject {
                root: tmp.path().to_path_buf(),
                title: "Novel".into(),
                kind: ProjectKind::Novel,
            })
            .unwrap();
        assert_eq!(events.len(), 1);

        core.execute(Command::CloseProject).unwrap();
        let events = core
            .execute(Command::OpenProject {
                root: tmp.path().to_path_buf(),
            })
            .unwrap();
        assert!(matches!(
            &events[0],
            Event::ProjectOpened {
                read_only: false,
                ..
            }
        ));

        let seen = seen.borrow().clone();
        assert!(matches!(seen[0], Event::ProjectCreated { .. }));
        assert!(matches!(seen[1], Event::ProjectClosed));
        assert!(matches!(seen[2], Event::ProjectOpened { .. }));
    }

    #[test]
    fn document_lifecycle_through_core() {
        let tmp = TempDir::new().unwrap();
        let mut core = opened_core(&tmp);

        let events = core
            .execute(Command::CreateDocument {
                kind: DocumentKind::Chapter,
                title: "First Chapter".into(),
            })
            .unwrap();
        let Event::DocumentCreated { rel, .. } = &events[0] else {
            panic!("expected DocumentCreated, got {events:?}");
        };
        let rel = rel.clone();

        core.execute(Command::AppendParagraph {
            doc: rel.clone(),
            text: "Hello world.".into(),
        })
        .unwrap();

        core.execute(Command::CloseProject).unwrap();
        core.execute(Command::OpenProject {
            root: tmp.path().to_path_buf(),
        })
        .unwrap();

        let doc = core.read_document(&rel).unwrap();
        assert!(doc.body().contains("Hello world."));
        assert!(doc.frontmatter.get_str("id").unwrap().starts_with("ch_"));
    }

    #[test]
    fn save_clears_stale_recovery_journal() {
        let tmp = TempDir::new().unwrap();
        let mut core = opened_core(&tmp);
        let events = core
            .execute(Command::CreateDocument {
                kind: DocumentKind::Chapter,
                title: "Drafty".into(),
            })
            .unwrap();
        let Event::DocumentCreated { rel, .. } = &events[0] else {
            panic!("expected DocumentCreated");
        };
        let rel = rel.clone();

        core.execute(Command::WriteRecovery {
            doc: rel.clone(),
            content: "unsaved".into(),
        })
        .unwrap();
        assert!(core.recovery_for(&rel).unwrap().is_some());

        let doc = core.read_document(&rel).unwrap();
        core.execute(Command::SaveDocument {
            doc: rel.clone(),
            raw: doc.raw,
        })
        .unwrap();
        assert!(
            core.recovery_for(&rel).unwrap().is_none(),
            "saving must drop the journal"
        );
    }

    #[test]
    fn commands_require_an_open_project() {
        let mut core = Core::new();
        assert!(
            core.execute(Command::CreateDocument {
                kind: DocumentKind::Note,
                title: "x".into(),
            })
            .is_err()
        );
        assert!(core.execute(Command::CloseProject).is_err());
    }

    #[test]
    fn read_only_project_rejects_mutations_with_reason() {
        let tmp = TempDir::new().unwrap();
        let _holder = opened_core(&tmp); // keeps the single-writer lock held
        let mut core = Core::new();
        let events = core
            .execute(Command::OpenProject {
                root: tmp.path().to_path_buf(),
            })
            .unwrap();
        match &events[0] {
            Event::ProjectOpened {
                read_only, reason, ..
            } => {
                assert!(*read_only);
                assert!(reason.as_deref().unwrap().contains("another session"));
            }
            other => panic!("expected ProjectOpened, got {other:?}"),
        }
        assert!(core.is_read_only());
        let err = core
            .execute(Command::CreateDocument {
                kind: DocumentKind::Chapter,
                title: "Nope".into(),
            })
            .unwrap_err()
            .to_string();
        assert!(err.contains("read-only"), "got: {err}");
    }

    #[test]
    fn resolve_and_document_queries() {
        let tmp = TempDir::new().unwrap();
        let mut core = opened_core(&tmp);
        let events = core
            .execute(Command::CreateDocument {
                kind: DocumentKind::Chapter,
                title: "Unique".into(),
            })
            .unwrap();
        let Event::DocumentCreated { rel, title } = &events[0] else {
            panic!("expected DocumentCreated");
        };
        let (rel, title) = (rel.clone(), title.clone());
        assert_eq!(core.resolve(&title).unwrap(), rel);
        assert_eq!(core.documents().len(), 1);
        assert_eq!(core.groups()[0].0, "manuscript");
        assert!(core.meta().unwrap().project_id().is_valid());
    }

    // ---- snapshots (Phase 6) -----------------------------------------------

    fn with_chapter(core: &mut Core, title: &str) -> PathBuf {
        let events = core
            .execute(Command::CreateDocument {
                kind: DocumentKind::Chapter,
                title: title.into(),
            })
            .unwrap();
        let Event::DocumentCreated { rel, .. } = &events[0] else {
            panic!("expected DocumentCreated");
        };
        rel.clone()
    }

    #[test]
    fn snapshot_create_restore_round_trip_through_core() {
        let tmp = TempDir::new().unwrap();
        let mut core = opened_core(&tmp);
        let rel = with_chapter(&mut core, "First");
        core.save_body(&rel, "The Arrival.\n").unwrap();

        assert!(core.snapshots().unwrap().is_empty(), "lazy store");

        let events = core
            .execute(Command::SnapshotCreate {
                label: Some("known good".into()),
            })
            .unwrap();
        let Event::SnapshotCreated { n, label, files } = &events[0] else {
            panic!("expected SnapshotCreated: {events:?}");
        };
        // project.yaml + the chapter are both authoring-visible files.
        assert_eq!((*n, label.as_str(), *files), (1, "known good", 2));

        // Break things, then restore.
        core.save_body(&rel, "garbage\n").unwrap();
        let events = core.execute(Command::SnapshotRestore { n: 1 }).unwrap();
        let Event::SnapshotRestored {
            n,
            restored,
            audio_differing,
            ..
        } = &events[0]
        else {
            panic!("expected SnapshotRestored: {events:?}");
        };
        assert_eq!(*n, 1);
        assert_eq!(*restored, 1);
        assert!(audio_differing.is_empty());
        assert_eq!(core.read_document(&rel).unwrap().body(), "The Arrival.\n");

        // Restoring saved the broken state first — history grew by one.
        let list = core.snapshots().unwrap();
        assert_eq!(list.len(), 2);
        assert!(
            list[1].label.contains("Before restoring snapshot #1"),
            "{:?}",
            list[1].label
        );
        // And the broken state is itself restorable.
        core.execute(Command::SnapshotRestore { n: 2 }).unwrap();
        assert_eq!(core.read_document(&rel).unwrap().body(), "garbage\n");
    }

    #[test]
    fn snapshot_restore_refuses_missing_and_zero() {
        let tmp = TempDir::new().unwrap();
        let mut core = opened_core(&tmp);
        with_chapter(&mut core, "First");
        core.execute(Command::SnapshotCreate { label: None })
            .unwrap();
        assert!(
            core.execute(Command::SnapshotRestore { n: 99 })
                .unwrap_err()
                .to_string()
                .contains("#99")
        );
        assert!(
            core.execute(Command::SnapshotRestore { n: 0 })
                .unwrap_err()
                .to_string()
                .contains("from 1")
        );
    }

    #[test]
    fn snapshot_diff_reports_working_changes() {
        let tmp = TempDir::new().unwrap();
        let mut core = opened_core(&tmp);
        let rel = with_chapter(&mut core, "First");
        core.save_body(&rel, "one\n").unwrap();
        core.execute(Command::SnapshotCreate { label: None })
            .unwrap();
        core.save_body(&rel, "one\ntwo\n").unwrap();

        let diff = core.snapshot_diff(1, None).unwrap();
        assert_eq!(diff.files.len(), 1);
        assert!(diff.files[0].patch.contains("+two"), "{:?}", diff.files[0]);
        assert_eq!(diff.against, None);
    }

    #[test]
    fn snapshot_ops_refuse_read_only_sessions() {
        let tmp = TempDir::new().unwrap();
        // The first session holds the lock (must stay alive for the test).
        let _core = opened_core(&tmp);
        // Simulate the lock being held elsewhere: reopen read-only by
        // taking a second Project on the same root through the future
        // format path — here the simplest faithful switch is a fresh core
        // opening while an original still holds the lock.
        let mut other = Core::new();
        other
            .execute(Command::OpenProject {
                root: tmp.path().to_path_buf(),
            })
            .unwrap();
        assert!(other.is_read_only(), "first core holds the lock");
        let err = other
            .execute(Command::SnapshotCreate { label: None })
            .unwrap_err()
            .to_string();
        assert!(err.contains("read-only"), "{err}");
        // Queries still work read-only; only mutations are refused.
        assert!(other.snapshots().unwrap().is_empty());
        assert!(other.execute(Command::SnapshotRestore { n: 1 }).is_err());
    }
}
