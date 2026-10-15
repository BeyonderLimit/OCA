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

/// Re-export so higher layers (story/music engines) can read the document
/// index through Core without depending on storage directly.
pub use oca_storage::index::DocEntry;
/// Re-export so frontends don't need a direct storage dependency for the
/// recovery type flowing through Core.
pub use oca_storage::journal::RecoveryEntry;

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
}

type Listener = Box<dyn FnMut(&Event)>;

#[derive(Default)]
pub struct Core {
    project: Option<Project>,
    listeners: Vec<Listener>,
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
        };

        for event in &events {
            for listener in &mut self.listeners {
                listener(event);
            }
        }
        Ok(events)
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
}
