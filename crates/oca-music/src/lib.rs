//! OCA music engine (Phase 3): the song model (`song.yaml` + `lyrics.md`),
//! ChordPro chords, and the deterministic lyric tools (rule 5) — syllable
//! counting, rhyme detection, repetition, section structure, plan drift.
//!
//! Layering (same shape as `oca-story`): the facade reads through `Core`
//! and mutates only through `Core` commands, so TUI/CLI stay thin clients
//! and every write is atomic + journaled. Nothing here is AI; later phases
//! build *on top* of these tools, never replace them.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use oca_core::{Command, Core};

pub mod check;
pub mod chord;
pub mod editor;
pub mod g2p;
pub mod library;
pub mod lyrics;
pub mod phonetics;
pub mod rhyme;
pub mod scores;
pub mod song;
pub mod syllable;
pub mod versions;

pub use check::{CheckReport, Drift, RepeatedLine, RhymeHit, SectionMetrics, StructureWarning};
pub use chord::{ChordEvent, parse_line as parse_chord_line, transpose, transpose_line};
pub use editor::{CursorMetrics, SectionMark, cursor_metrics, section_marks};
pub use library::{Album, Artist};
pub use lyrics::{LyricLine, Lyrics, Section, SectionKind};
pub use phonetics::{EnglishPhonetics, PhoneticProvider, Pronunciation};
pub use rhyme::{InternalRhyme, RhymeKind, RhymePair, classify, internal_rhymes, rhymes_with};
pub use scores::{LyricScores, rhyme_scheme, score_lines};
pub use song::{SongEntry, SongYaml};
pub use syllable::{LineMetrics, line_metrics, line_syllables, meter};
pub use versions::VersionRef;

/// `music/songs/<slug>/lyrics.md` is the discovery signal for a song.
const SONGS_DIR: &str = "music";

/// Read-side facade over an open project.
pub struct Music<'c> {
    core: &'c Core,
}

/// A song with both files loaded.
#[derive(Clone, Debug)]
pub struct LoadedSong {
    pub entry: SongEntry,
    pub yaml: SongYaml,
    pub lyrics: Lyrics,
    /// Language the lyric tools run under (song override → project).
    pub language: String,
}

impl<'c> Music<'c> {
    pub fn new(core: &'c Core) -> Self {
        Self { core }
    }

    /// Every song directory that has a `lyrics.md`, in path order.
    pub fn songs(&self) -> Result<Vec<SongEntry>> {
        let project = self.core.project().context("no project is open")?;
        let mut dirs: Vec<PathBuf> = project
            .documents()
            .iter()
            .filter(|d| d.group() == SONGS_DIR)
            .filter_map(|d| {
                let mut comps = d.rel.components();
                match (comps.next(), comps.next(), comps.next(), comps.next()) {
                    (Some(a), Some(b), Some(c), Some(d4))
                        if a.as_os_str() == "music"
                            && b.as_os_str() == "songs"
                            && d4.as_os_str() == "lyrics.md" =>
                    {
                        Some(PathBuf::from("music").join(b).join(c))
                    }
                    _ => None,
                }
            })
            .collect();
        dirs.sort();
        dirs.dedup();
        Ok(dirs
            .into_iter()
            .map(|dir_rel| SongEntry {
                yaml_rel: dir_rel.join("song.yaml"),
                lyrics_rel: dir_rel.join("lyrics.md"),
                dir_rel,
            })
            .collect())
    }

    /// Resolve a song by slug (`ashes`), id (`song_ashes`), or title
    /// (case-insensitive) — the forms an author actually types.
    pub fn song(&self, key: &str) -> Result<LoadedSong> {
        let entries = self.songs()?;
        let root = self.core.project().context("no project is open")?.root();
        let lower = key.to_lowercase();
        let mut found: Option<(SongEntry, SongYaml)> = None;
        for entry in &entries {
            let yaml_raw = std::fs::read_to_string(root.join(&entry.yaml_rel))
                .with_context(|| format!("read {}", entry.yaml_rel.display()))?;
            let yaml = SongYaml::parse(&yaml_raw)
                .with_context(|| format!("parse {}", entry.yaml_rel.display()))?;
            let title = yaml.title.clone().unwrap_or_default().to_lowercase();
            if entry.slug() == key || yaml.id.as_deref() == Some(key) || title == lower {
                found = Some((entry.clone(), yaml));
                break;
            }
        }
        let Some((entry, yaml)) = found else {
            bail!("no song {key:?} — try a slug (ashes), an id (song_ashes), or a title");
        };
        let doc = self.core.read_document(&entry.lyrics_rel)?;
        let lyrics = lyrics::Lyrics::parse(&doc.raw)
            .with_context(|| format!("parse {}", entry.lyrics_rel.display()))?;
        let project_language = self.core.project().unwrap().meta().language();
        let language = yaml.effective_language(&project_language);
        Ok(LoadedSong {
            entry,
            yaml,
            lyrics,
            language,
        })
    }

    /// Deterministic report: drift, structure, syllables, repetition,
    /// end rhymes.
    pub fn check(&self, key: &str) -> Result<CheckReport> {
        let song = self.song(key)?;
        let project_language = self.core.project().unwrap().meta().language();
        Ok(check::check(&song.yaml, &song.lyrics, &project_language))
    }

    /// Deterministic scores for one block of this song's lyric lines —
    /// what the AI proposal review shows for both the current draft and
    /// the revision (Phase 10, `OCA-build.md` §7.3). The language comes
    /// from the song (override → project), never guessed.
    pub fn score_block(&self, key: &str, text: &str) -> Result<LyricScores> {
        let song = self.song(key)?;
        let lines: Vec<String> = text.lines().map(str::to_string).collect();
        Ok(scores::score_lines(&lines, &song.language))
    }

    /// `music/artist.yaml`, when present.
    pub fn artist(&self) -> Result<Option<Artist>> {
        let root = self.core.project().context("no project is open")?.root();
        Artist::load(root)
    }

    /// Every album under `music/albums/`, in directory order.
    pub fn albums(&self) -> Result<Vec<(String, Album)>> {
        let root = self.core.project().context("no project is open")?.root();
        Album::load_all(root)
    }

    /// Saved versions of a song (files + labels).
    pub fn versions(&self, key: &str) -> Result<Vec<VersionRef>> {
        let song = self.song(key)?;
        let root = self.core.project().context("no project is open")?.root();
        versions::list(&root.join(&song.entry.dir_rel))
    }

    /// Is this project-relative path a song's `lyrics.md`? (The TUI asks
    /// before wiring up section navigation + status-bar metrics.)
    pub fn is_lyrics(rel: &Path) -> bool {
        let mut c = rel.components();
        matches!(
            (c.next(), c.next(), c.next(), c.next()),
            (Some(a), Some(b), Some(c), Some(d))
                if a.as_os_str() == "music"
                    && b.as_os_str() == "songs"
                    && d.as_os_str() == "lyrics.md"
                    && !c.as_os_str().is_empty()
        ) && rel.components().count() == 4
    }

    /// Language for a lyrics file (song.yaml override → project default).
    /// `None` when `rel` is not a song's lyrics file.
    pub fn lyrics_language(&self, rel: &Path) -> Result<Option<String>> {
        if !Self::is_lyrics(rel) {
            return Ok(None);
        }
        let root = self.core.project().context("no project is open")?.root();
        let project_language = self.core.project().unwrap().meta().language();
        let yaml_path = root.join(rel).with_file_name("song.yaml");
        let lang = if yaml_path.is_file() {
            let raw = std::fs::read_to_string(&yaml_path)
                .with_context(|| format!("read {}", yaml_path.display()))?;
            SongYaml::parse(&raw)
                .with_context(|| format!("parse {}", yaml_path.display()))?
                .effective_language(&project_language)
        } else {
            project_language
        };
        Ok(Some(lang))
    }

    /// Phonetics for live editor metrics: user `dictionary.yaml` overrides
    /// plus the embedded dictionary and rule fallback (A9).
    pub fn phonetics(&self) -> Result<EnglishPhonetics> {
        let root = self.core.project().context("no project is open")?.root();
        EnglishPhonetics::for_project(root)
    }

    // ---- writes (always through Core commands: atomic + journaled) -------

    /// Create `music/songs/<slug>/` with a skeleton `song.yaml` and
    /// `lyrics.md` — the "write a complete song offline" starting point.
    pub fn new_song(core: &mut Core, slug: &str, title: &str) -> Result<SongEntry> {
        if slug.is_empty()
            || !slug
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            bail!("song slug must be lowercase letters/digits/dashes, got {slug:?}");
        }
        let project = core.project().context("no project is open")?;
        let language = project.meta().language();
        let dir_rel = PathBuf::from(SONGS_DIR).join("songs").join(slug);
        let entry = SongEntry {
            yaml_rel: dir_rel.join("song.yaml"),
            lyrics_rel: dir_rel.join("lyrics.md"),
            dir_rel,
        };
        let id = format!("song_{}", slug.replace('-', "_"));
        let yaml_raw = song::skeleton_yaml(&id, title, &language);
        let lyrics_raw = song::skeleton_lyrics(title);
        core.execute(Command::SaveDocument {
            doc: entry.yaml_rel.clone(),
            raw: yaml_raw,
        })?;
        core.execute(Command::SaveDocument {
            doc: entry.lyrics_rel.clone(),
            raw: lyrics_raw,
        })?;
        Ok(entry)
    }

    /// "Save as version": copy `lyrics.md` byte-for-byte into
    /// `versions/vNN.md` (next number), optionally recording a label in
    /// `versions/index.yaml`. Compare/promote arrive in Phase 6.
    pub fn save_version(core: &mut Core, key: &str, label: Option<&str>) -> Result<VersionRef> {
        let (root, song) = {
            let music = Music::new(core);
            let root = music
                .core
                .project()
                .context("no project is open")?
                .root()
                .to_path_buf();
            (root, music.song(key)?)
        };
        let n = versions::next_version_number(&root.join(&song.entry.dir_rel))?;
        let raw = core.read_document(&song.entry.lyrics_rel)?.raw.to_string();
        let (version_rel, index_rel) = versions::save_paths(&song.entry.dir_rel, n);
        core.execute(Command::SaveDocument {
            doc: version_rel.clone(),
            raw,
        })?;
        let file = version_rel
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_string();
        let saved = VersionRef {
            file: file.clone(),
            label: label.map(str::to_string),
        };
        if let Some(label) = label {
            let index = versions::read_index(&root.join(&song.entry.dir_rel))?;
            let index = versions::upsert_label(&index, &file, label);
            core.execute(Command::SaveDocument {
                doc: index_rel,
                raw: versions::render_index(&index)?,
            })?;
        }
        Ok(saved)
    }
}
