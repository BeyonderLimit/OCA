//! `oca-audio` — recording, playback, and take management without AI.
//!
//! Layering (implementation plan §2): this sits above the story and music
//! engines, which it uses to resolve attach targets ("attach to chapter"
//! validates the chapter exists and stores its immutable id). Device I/O
//! is abstracted behind [`AudioHost`] so every rule here is testable
//! without hardware ([`mock::MockHost`]).
//!
//! Files are canonical (rule 2): a take is `<stem>.wav` + `<stem>.yaml`
//! sidecar under `audio/` or `music/songs/<song>/demos/`. Nothing is
//! stored in the database yet — the index arrives in Phase 5.

pub mod host;
pub mod mock;
pub mod play;
pub mod record;
pub mod store;
pub mod take;
pub mod wav;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use oca_core::Core;
use oca_music::Music;
use oca_story::Story;

pub use host::{AudioHost, CaptureStream, CpalHost, PlaybackStream};
pub use mock::MockHost;
pub use play::Player;
pub use record::{RecState, RecordedAudio, Recorder};
pub use store::TakeStore;
pub use take::{Attachment, Note, Take, TakeMeta};
pub use wav::{AudioSpec, DecodedAudio};

/// Audio subsystem over an open project: the facade TUI and CLI talk to.
pub struct Audio<'c> {
    core: &'c Core,
    host: Arc<dyn AudioHost>,
    store: TakeStore,
}

/// What the devices look like right now (for `oca doctor`): the format
/// record/playback would run at, or why that direction is unavailable.
#[derive(Debug)]
pub struct DeviceReport {
    pub input: std::result::Result<AudioSpec, anyhow::Error>,
    pub output: std::result::Result<AudioSpec, anyhow::Error>,
}

impl DeviceReport {
    pub fn available(&self) -> bool {
        self.input.is_ok() && self.output.is_ok()
    }
}

impl<'c> Audio<'c> {
    pub fn new(core: &'c Core, host: Arc<dyn AudioHost>) -> Result<Self> {
        let root = core
            .project()
            .context("no project is open")?
            .root()
            .to_path_buf();
        Ok(Self {
            core,
            host,
            store: TakeStore::new(root),
        })
    }

    pub fn store(&self) -> &TakeStore {
        &self.store
    }

    pub fn takes(&self) -> Result<Vec<Take>> {
        self.store.list()
    }

    pub fn take(&self, key: &str) -> Result<Take> {
        self.store.resolve(key)
    }

    /// Start recording from the default input device.
    pub fn record(&self) -> Result<Recorder> {
        self.writable("record")?;
        Recorder::start(self.host.as_ref())
    }

    /// Save a finished recording as a new take in `dir_rel`.
    pub fn save_take(
        &self,
        audio: &RecordedAudio,
        dir_rel: &Path,
        title: Option<&str>,
    ) -> Result<Take> {
        self.writable("save takes")?;
        self.store.save(dir_rel, audio, title)
    }

    /// Play a take through the default output device. Dropping the player
    /// stops playback.
    pub fn play(&self, take: &Take) -> Result<Player> {
        let audio = self.store.load(take)?;
        Player::start(self.host.as_ref(), audio)
    }

    pub fn rename(&self, take: &Take, new_stem: &str) -> Result<Take> {
        self.writable("rename takes")?;
        self.store.rename(take, new_stem)
    }

    pub fn delete(&self, take: &Take) -> Result<()> {
        self.writable("delete takes")?;
        self.store.delete(take)
    }

    pub fn attach(&self, take: &Take, target: &Attachment) -> Result<Take> {
        self.writable("attach takes")?;
        self.store.attach(take, Some(target))
    }

    pub fn detach(&self, take: &Take) -> Result<Take> {
        self.writable("attach takes")?;
        self.store.attach(take, None)
    }

    /// Merge fields into a take's Creative Note (an all-empty note clears
    /// the block). Existing fields — including reserved melody data —
    /// survive; only the fields you pass are changed.
    pub fn update_note(
        &self,
        take: &Take,
        text: Option<&str>,
        lyrics: Option<&str>,
        tempo_bpm: Option<u32>,
    ) -> Result<Take> {
        self.writable("edit takes")?;
        let mut note = take.meta.note.clone().unwrap_or_default();
        if let Some(text) = text {
            note.text = Some(text.to_string());
        }
        if let Some(lyrics) = lyrics {
            note.lyrics = Some(lyrics.to_string());
        }
        if let Some(tempo) = tempo_bpm {
            note.tempo_bpm = Some(tempo);
        }
        self.store.set_note(take, Some(&note))
    }

    /// Parse an attach target spec: `project`, `chapter:<chapter>`, or
    /// `song:<song>` — the forms a human types. Chapter and song keys are
    /// resolved (and validated) through their engines right here.
    pub fn resolve_target(&self, spec: &str) -> Result<Attachment> {
        if spec == "project" {
            return Ok(Attachment::Project);
        }
        let Some((kind, key)) = spec.split_once(':') else {
            bail!("attach target must be `project`, `chapter:<chapter>`, or `song:<song>`");
        };
        match kind {
            "chapter" => {
                let chapter = Story::new(self.core).chapter(key)?;
                Ok(Attachment::Chapter { id: chapter.id })
            }
            "song" => {
                let song = Music::new(self.core).song(key)?;
                let id =
                    song.yaml.id.clone().context(
                        "song.yaml has no `id:` — add one so audio links survive renames",
                    )?;
                Ok(Attachment::Song { id })
            }
            other => bail!(
                "unknown attach target {other:?} — use `project`, `chapter:<chapter>`, or `song:<song>`"
            ),
        }
    }

    /// Device availability for `oca doctor`: both directions probed
    /// independently so a machine with (say) headphones-only still gets
    /// credit for what works.
    pub fn probe(host: &dyn AudioHost) -> DeviceReport {
        DeviceReport {
            input: host.default_input(),
            output: host.default_output(),
        }
    }

    fn writable(&self, op: &str) -> Result<()> {
        if self.core.is_read_only() {
            let reason = self
                .core
                .read_only_reason()
                .unwrap_or_else(|| "read-only project".into());
            bail!("cannot {op}: {reason}");
        }
        Ok(())
    }
}

/// Default take directories, for callers that offer a choice.
pub fn default_take_dir() -> PathBuf {
    PathBuf::from("audio")
}
