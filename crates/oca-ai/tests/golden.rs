//! Golden prompt tests (Phase 8, exit criteria): one file per context
//! type — the byte-exact system + user messages the PromptBuilder renders
//! from the sample project. Regenerate after an intentional change with
//! `OCA_UPDATE_GOLDEN=1 cargo test -p oca-ai` and eyeball the diff; the
//! files are reviewed artifacts, not disposable output.

use std::path::{Path, PathBuf};

use anyhow::Result;
use oca_ai::{
    IntoContext, PromptBuilder, Retriever,
    context::{
        AlbumContext, ArtistContext, ChapterContext, CharacterContext, ManuscriptContext,
        SceneContext, SongContext, WorldContext,
    },
};
use oca_core::{Command, Core};
use tempfile::TempDir;

/// The fixed task every golden uses, so prompt diffs are context-only.
const TASK: &str = "Task: draft the next paragraph in the project's voice.";
/// Generous budget: goldens capture the untruncated render.
const GOLDEN_BUDGET: usize = 8000;

fn sample() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/blackwood")
}

/// Copy the sample novel and add the material it lacks (world, artist,
/// album, a chapter with scenes) so every context type has golden input.
fn fixture() -> Result<TempDir> {
    let tmp = TempDir::new()?;
    copy_dir(&sample(), tmp.path())?;
    std::fs::create_dir_all(tmp.path().join("world"))?;
    std::fs::write(
        tmp.path().join("world/the-hall.md"),
        "---\nid: loc_hall\ntitle: The Blackwood Hall\nkind: location\n---\n\nThe \
         hall has stood above the harbor for two hundred years. Its beams are \
         black with smoke, and the elders sit with their backs to the sea.\n",
    )?;
    std::fs::write(
        tmp.path().join("music/artist.yaml"),
        "name: Mara Voss\ngenres: [folk, ambient]\ninfluences: [Nick Drake, Vashti \
         Bunyan]\nvocal_style: breathy alto\nthemes: [grief, homecoming]\n\
         instrumentation: [nylon guitar, upright bass]\ntempos: slow to mid\n\
         production: dry and close-mic'd\nforbidden_cliches: [broken heart]\n\
         goals: keep the coastal imagery\n",
    )?;
    std::fs::create_dir_all(tmp.path().join("music/albums/debut"))?;
    std::fs::write(
        tmp.path().join("music/albums/debut/album.yaml"),
        "id: album_debut\ntitle: Tales from the Coast\nconcept: Ten songs about \
         leaving and returning.\ntracks:\n  - Ashes\n",
    )?;
    std::fs::write(
        tmp.path().join("manuscript/011-the-storm.md"),
        "---\nid: ch_011\ntitle: The Storm\n---\n\n# The Storm\n\n## Before the \
         Wind\n\nThe wind came up from the south before noon, and the gulls went \
         quiet first. Elena tied the boat twice and wished she had tied it three \
         times.\n\n## After the Rain\n\nBy evening the road ran with water and the \
         hall smelled of wet wool. Nobody spoke of the boat.\n",
    )?;
    Ok(tmp)
}

fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let name = entry.file_name();
        let dest = to.join(&name);
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &dest)?;
        } else {
            std::fs::copy(entry.path(), &dest)?;
        }
    }
    Ok(())
}

/// Open the fixture and index it once (search needs the cache DB).
fn open(fixture: &TempDir) -> Result<Core> {
    let mut core = Core::new();
    core.execute(Command::OpenProject {
        root: fixture.path().to_path_buf(),
    })?;
    core.reindex()?;
    Ok(core)
}

fn golden(name: &str, rendered: &str) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden");
    let path = dir.join(format!("{name}.txt"));
    if std::env::var_os("OCA_UPDATE_GOLDEN").is_some() {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, rendered).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "missing golden {} ({e}) — regenerate with OCA_UPDATE_GOLDEN=1 \
             cargo test -p oca-ai",
            path.display()
        )
    });
    assert_eq!(
        rendered, expected,
        "golden prompt {name} drifted — regenerate deliberately and review"
    );
}

fn render(name: &str, context: &impl IntoContext) {
    let prompt = PromptBuilder::new(GOLDEN_BUDGET)
        .build_context(TASK, context)
        .unwrap();
    assert!(
        prompt.dropped == 0 && prompt.truncated == 0,
        "{name}: budget too small"
    );
    assert!(!prompt.manifest.is_empty(), "{name}: manifest empty");
    golden(
        name,
        &format!(
            "--- system ---\n{}\n--- user ---\n{}\n",
            prompt.system, prompt.user
        ),
    );
}

#[test]
fn golden_manuscript_context() -> Result<()> {
    let tmp = fixture()?;
    let core = open(&tmp)?;
    let ctx = Retriever::new(&core).manuscript()?;
    assert!(ctx.title.contains("Blackwood"), "{}", ctx.title);
    assert_eq!(ctx.chapters.len(), 11, "10 sample chapters + the storm");
    render("manuscript", &ctx);
    Ok(())
}

#[test]
fn golden_chapter_context() -> Result<()> {
    let tmp = fixture()?;
    let core = open(&tmp)?;
    let ctx = Retriever::new(&core).chapter("2")?;
    assert_eq!(ctx.chapter.id, "ch_002");
    assert_eq!(ctx.previous.as_ref().map(|c| c.id.as_str()), Some("ch_001"));
    assert_eq!(ctx.next.as_ref().map(|c| c.id.as_str()), Some("ch_003"));
    render("chapter", &ctx);
    Ok(())
}

#[test]
fn golden_scene_context() -> Result<()> {
    let tmp = fixture()?;
    let core = open(&tmp)?;
    let ctx = Retriever::new(&core).scene("11", "After the Rain")?;
    assert!(
        ctx.body.contains("the hall smelled of wet wool"),
        "{}",
        ctx.body
    );
    assert!(
        !ctx.body.contains("Before the Wind"),
        "scene must end at the next `## `"
    );
    render("scene", &ctx);
    Ok(())
}

#[test]
fn golden_character_context() -> Result<()> {
    let tmp = fixture()?;
    let core = open(&tmp)?;
    let ctx = Retriever::new(&core).character("Elena", None)?;
    assert_eq!(ctx.name, "Elena");
    assert!(!ctx.state.is_empty(), "carry-forward state resolves");
    render("character", &ctx);
    Ok(())
}

#[test]
fn golden_world_context() -> Result<()> {
    let tmp = fixture()?;
    let core = open(&tmp)?;
    let ctx = Retriever::new(&core).world()?;
    assert_eq!(ctx.entities.len(), 1);
    assert_eq!(ctx.entities[0].id, "loc_hall");
    render("world", &ctx);
    Ok(())
}

#[test]
fn golden_song_context() -> Result<()> {
    let tmp = fixture()?;
    let core = open(&tmp)?;
    let ctx = Retriever::new(&core).song("ashes")?;
    assert!(!ctx.sections.is_empty());
    render("song", &ctx);
    Ok(())
}

#[test]
fn golden_album_context() -> Result<()> {
    let tmp = fixture()?;
    let core = open(&tmp)?;
    let ctx = Retriever::new(&core).album("debut")?;
    assert_eq!(ctx.tracks, vec!["Ashes".to_string()]);
    render("album", &ctx);
    Ok(())
}

#[test]
fn golden_artist_context() -> Result<()> {
    let tmp = fixture()?;
    let core = open(&tmp)?;
    let ctx = Retriever::new(&core).artist()?;
    assert_eq!(ctx.name.as_deref(), Some("Mara Voss"));
    render("artist", &ctx);
    Ok(())
}

#[test]
fn budget_is_enforced_with_truncation_and_manifest() -> Result<()> {
    let tmp = fixture()?;
    let core = open(&tmp)?;
    let r = Retriever::new(&core);
    let chapter = r.chapter("1")?;
    let world = r.world()?;
    let mut items = chapter.blocks();
    items.extend(world.blocks());
    // Small enough that chapter body + world facts can't both fit whole
    // (the fixed preamble + task alone are ~140 tokens).
    let prompt = PromptBuilder::new(240).build(TASK, &items)?;
    assert!(
        prompt.estimated_tokens <= 240,
        "{}",
        prompt.estimated_tokens
    );
    assert!(prompt.truncated + prompt.dropped > 0);
    if prompt.truncated > 0 {
        assert!(prompt.user.contains("… [truncated]"));
    }
    // The manifest lists exactly what is in the prompt.
    let wrapped = prompt.manifest.len();
    assert_eq!(
        prompt.user.matches("<context ").count() + prompt.user.matches("<untrusted-source").count(),
        wrapped
    );
    Ok(())
}

#[test]
fn search_items_are_untrusted_context() -> Result<()> {
    let tmp = fixture()?;
    let core = open(&tmp)?;
    let items = Retriever::new(&core).search("blackwood", 5)?;
    assert!(!items.is_empty(), "the sample matches 'blackwood'");
    assert!(items.iter().all(|i| i.untrusted));
    let prompt = PromptBuilder::new(2000).build(TASK, &items)?;
    assert!(prompt.user.contains("<untrusted-source"));
    assert!(!prompt.user.contains("<context "));
    Ok(())
}

#[test]
fn every_context_type_is_serializable() -> Result<()> {
    let tmp = fixture()?;
    let core = open(&tmp)?;
    let r = Retriever::new(&core);
    let manuscript: ManuscriptContext = r.manuscript()?;
    let chapter: ChapterContext = r.chapter("3")?;
    let scene: SceneContext = r.scene("11", "Before the Wind")?;
    let character: CharacterContext = r.character("Marcus", Some("2"))?;
    let world: WorldContext = r.world()?;
    let song: SongContext = r.song("ashes")?;
    let album: AlbumContext = r.album("debut")?;
    let artist: ArtistContext = r.artist()?;
    for (name, json) in [
        ("manuscript", serde_json::to_string(&manuscript)?),
        ("chapter", serde_json::to_string(&chapter)?),
        ("scene", serde_json::to_string(&scene)?),
        ("character", serde_json::to_string(&character)?),
        ("world", serde_json::to_string(&world)?),
        ("song", serde_json::to_string(&song)?),
        ("album", serde_json::to_string(&album)?),
        ("artist", serde_json::to_string(&artist)?),
    ] {
        assert!(!json.is_empty(), "{name} rendered empty");
        // Round-trips through JSON.
        let back: serde_json::Value = serde_json::from_str(&json)?;
        assert!(back.is_object(), "{name}");
    }
    Ok(())
}
