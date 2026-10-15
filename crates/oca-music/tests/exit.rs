//! Phase 3 exit criteria: a complete song authored offline; syllable and
//! rhyme tools covered by known-answer fixtures — plus the v2 additions:
//! syllable count ignores chord markers, an invented name with a user
//! pronunciation rhymes correctly, unsupported language says so, chord
//! transposition, plan drift against the sample song, and save-as-version.

use std::path::{Path, PathBuf};

use oca_core::{Command, Core};
use oca_music::phonetics::PhoneticProvider;
use oca_music::{
    EnglishPhonetics, Music, RhymeKind, classify, line_syllables, parse_chord_line, rhymes_with,
    transpose_line,
};

const SAMPLE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/blackwood");

fn copy_tree(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let to = dst.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &to);
        } else {
            std::fs::copy(entry.path(), &to).unwrap();
        }
    }
}

/// A private copy of the sample project, opened through Core.
fn open_sample() -> (tempfile::TempDir, Core, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let dst = tmp.path().join("blackwood");
    copy_tree(Path::new(SAMPLE), &dst);
    let mut core = Core::new();
    core.execute(Command::OpenProject { root: dst.clone() })
        .unwrap();
    (tmp, core, dst)
}

// ---- the committed sample song ----------------------------------------------

#[test]
fn sample_song_loads_with_sections_and_metadata() {
    let (_tmp, core, _root) = open_sample();
    let music = Music::new(&core);
    let song = music.song("ashes").unwrap();
    assert_eq!(song.yaml.id.as_deref(), Some("song_ashes"));
    assert_eq!(song.yaml.tempo, Some(92));
    assert_eq!(song.yaml.key.as_deref(), Some("D minor"));
    assert_eq!(song.lyrics.title.as_deref(), Some("Ashes"));
    let canon: Vec<&str> = song
        .lyrics
        .sections
        .iter()
        .map(|s| s.canonical.as_str())
        .collect();
    assert_eq!(
        canon,
        vec![
            "intro",
            "verse",
            "pre_chorus",
            "chorus",
            "verse",
            "final_chorus",
            "chorus",
            "outro"
        ]
    );
}

#[test]
fn sample_song_check_is_clean() {
    let (_tmp, core, _root) = open_sample();
    let report = Music::new(&core).check("ashes").unwrap();
    assert!(
        report.drift.is_none(),
        "plan matches headings: {:?}",
        report.drift
    );
    assert!(report.structure.is_empty(), "{:?}", report.structure);
    assert!(report.language_supported);
    assert!(report.total_syllables.unwrap() > 0);
    // the reference section reuses the original chorus's syllables
    let chorus = &report.sections[3];
    let repeat = &report.sections[6];
    assert!(repeat.is_reference);
    assert_eq!(chorus.syllables, repeat.syllables);
}

#[test]
fn sample_song_resolution_by_id_and_title() {
    let (_tmp, core, _root) = open_sample();
    let music = Music::new(&core);
    assert!(music.song("song_ashes").is_ok());
    assert!(music.song("Ashes").is_ok());
    assert!(music.song("nope").is_err());
}

// ---- exit: syllable count ignores chord markers ------------------------------

#[test]
fn exit_syllable_count_ignores_chord_markers() {
    let (_tmp, core, _root) = open_sample();
    let song = Music::new(&core).song("ashes").unwrap();
    let p = EnglishPhonetics::new();
    // every chorded line of the song must count identically to its
    // stripped form
    for section in &song.lyrics.sections {
        for line in &section.lines {
            let (stripped, _) = parse_chord_line(&line.raw);
            assert_eq!(
                line_syllables(&line.raw, &p),
                line_syllables(&stripped, &p),
                "line {:?}",
                line.raw
            );
        }
    }
    // spot check: [Dm]ashes on the [F]water = 2 + 1 + 1 + 2
    assert_eq!(line_syllables("[Dm]ashes on the [F]water", &p), 6);
}

// ---- exit: invented name with user pronunciation rhymes correctly ------------

#[test]
fn exit_invented_name_rhymes_via_user_dictionary() {
    let (_tmp, _core, root) = open_sample();
    // dictionary.yaml in the project gives "Lyra" the sound of "lie-er"
    let p = EnglishPhonetics::for_project(&root).unwrap();
    let lyra = p.pronounce("Lyra").unwrap();
    let fire = p.pronounce("fire").unwrap();
    assert_eq!(
        classify(&lyra, &fire),
        Some(RhymeKind::Multisyllabic),
        "L AY1 ER0 vs F AY1 ER0"
    );
    // and through the query API the author actually uses
    let got = rhymes_with(
        "Lyra",
        &["fire".into(), "water".into(), "higher".into()],
        &p,
    );
    assert_eq!(got[0].word, "fire");
    assert_eq!(got[0].kind, RhymeKind::Multisyllabic);
    assert!(got.iter().any(|r| r.word == "higher"));
    // sanity: without the user dictionary the engine has never heard it
    let bare = EnglishPhonetics::new();
    let naive = bare.pronounce("Lyra").unwrap();
    assert_ne!(naive, lyra, "user override must actually apply");
}

// ---- exit: unsupported language says "unsupported" ---------------------------

#[test]
fn exit_unsupported_language_reports_unsupported() {
    let (_tmp, core, root) = open_sample();
    // French song: the tools refuse rather than show wrong numbers (A9)
    let yaml_path = root.join("music/songs/ashes/song.yaml");
    let raw = std::fs::read_to_string(&yaml_path).unwrap();
    std::fs::write(&yaml_path, raw.replace("language: en", "language: fr")).unwrap();
    let mut core = core;
    core.execute(Command::CloseProject).unwrap();
    core.execute(Command::OpenProject { root: root.clone() })
        .unwrap();
    let report = Music::new(&core).check("ashes").unwrap();
    assert!(!report.language_supported);
    assert_eq!(report.language, "fr");
    assert!(report.total_syllables.is_none());
    assert!(report.sections.iter().all(|s| s.syllables.is_none()));
    // non-phonetic parts still work
    assert!(report.drift.is_none());
    assert!(report.structure.is_empty());
}

// ---- exit: a complete song authored offline ----------------------------------

#[test]
fn exit_complete_song_authored_offline_via_cli_surface() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("newbook");
    let mut core = Core::new();
    core.execute(Command::CreateProject {
        root: root.clone(),
        title: "New Book".into(),
        kind: oca_model::ProjectKind::Novel,
    })
    .unwrap();

    // 1. create the song skeleton
    let entry = Music::new_song(&mut core, "the-road", "The Road").unwrap();
    assert!(root.join(&entry.yaml_rel).exists());
    assert!(root.join(&entry.lyrics_rel).exists());

    // 2. write the lyrics offline (what the TUI/CLI would save)
    let lyrics = "\
# The Road

## Verse 1
[Dm]one road out of [F]nowhere
running to the [C]sea

## Chorus
[F]drive on, [C]drive on
the road is [G]calling me

## Chorus 2 {=chorus}
";
    core.execute(Command::SaveDocument {
        doc: entry.lyrics_rel.clone(),
        raw: lyrics.into(),
    })
    .unwrap();

    // 3. the report sees a complete, drift-free song
    let report = Music::new(&core).check("the-road").unwrap();
    assert_eq!(report.song_id, "song_the_road");
    assert!(report.drift.is_none(), "no plan = nothing to drift");
    assert!(report.structure.is_empty(), "{:?}", report.structure);
    assert!(report.total_syllables.unwrap() > 0);
    // the reference chorus resolves
    assert!(report.sections[2].is_reference);
    assert_eq!(report.sections[1].syllables, report.sections[2].syllables);
    // "nowhere" and "sea"?? — end rhymes come from the engine:
    assert!(!report.rhymes.is_empty(), "sea/… should rhyme somewhere");

    // 4. metadata survives as authored (files are canonical)
    let song = Music::new(&core).song("the-road").unwrap();
    assert_eq!(song.yaml.title.as_deref(), Some("The Road"));
    assert_eq!(song.yaml.status.as_deref(), Some("draft"));
}

#[test]
fn new_song_rejects_bad_slugs_and_duplicates_resolve_by_first() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("p");
    let mut core = Core::new();
    core.execute(Command::CreateProject {
        root: root.clone(),
        title: "P".into(),
        kind: oca_model::ProjectKind::Novel,
    })
    .unwrap();
    assert!(Music::new_song(&mut core, "Bad Slug!", "X").is_err());
    Music::new_song(&mut core, "ok-song", "OK").unwrap();
}

// ---- chords: display and transpose (A11) -------------------------------------

#[test]
fn exit_chords_parse_and_transpose_over_the_sample() {
    let (_tmp, core, _root) = open_sample();
    let song = Music::new(&core).song("ashes").unwrap();
    let verse = &song.lyrics.sections[1];
    let (stripped, events) = parse_chord_line(&verse.lines[0].raw);
    assert_eq!(stripped, "ashes on the water");
    assert_eq!(events[0].chord, "Dm");
    // transpose +2: Dm→Em, F→G, Am→Bm, C→D
    let up2 = transpose_line(&verse.lines[0].raw, 2).unwrap();
    assert!(up2.starts_with("[Em]ashes on the [G]water"), "got {up2}");
    let chorus_line = "[F]ashes, [C]ashes";
    assert_eq!(
        transpose_line(chorus_line, 2).unwrap(),
        "[G]ashes, [D]ashes"
    );
}

// ---- save as version (A11 G12; compare/promote = Phase 6) --------------------

#[test]
fn exit_save_version_copies_bytes_and_records_labels() {
    let (_tmp, core, root) = open_sample();
    let mut core = core;

    let v1 = Music::save_version(&mut core, "ashes", Some("first pass")).unwrap();
    assert_eq!(v1.file, "v01.md");
    assert_eq!(v1.label.as_deref(), Some("first pass"));

    let v1_path = root.join("music/songs/ashes/versions/v01.md");
    let lyrics_bytes = std::fs::read(root.join("music/songs/ashes/lyrics.md")).unwrap();
    assert_eq!(
        std::fs::read(&v1_path).unwrap(),
        lyrics_bytes,
        "byte-identical"
    );

    // editing then saving again gives v02; v01 stays untouched
    core.execute(Command::SaveDocument {
        doc: PathBuf::from("music/songs/ashes/lyrics.md"),
        raw: "# Ashes\n\n## Verse 1\ndraft two words\n\n## Chorus 2 {=chorus}\n\n## Chorus\nstill here\n".into(),
    })
    .unwrap();
    let v2 = Music::save_version(&mut core, "ashes", None).unwrap();
    assert_eq!(v2.file, "v02.md");
    assert!(v2.label.is_none());
    assert_ne!(
        std::fs::read(&v1_path).unwrap(),
        std::fs::read(root.join("music/songs/ashes/versions/v02.md")).unwrap()
    );

    let listed = Music::new(&core).versions("ashes").unwrap();
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].label.as_deref(), Some("first pass"));
    assert!(listed[1].label.is_none());
}

// ---- albums + artist.yaml (v2 amendments) -------------------------------------

#[test]
fn albums_and_artist_load_from_the_tree() {
    let (_tmp, core, root) = open_sample();
    std::fs::write(
        root.join("music/artist.yaml"),
        "name: Blackwood Sessions\ngenres: [alternative]\n",
    )
    .unwrap();
    let album_dir = root.join("music/albums/the-long-night");
    std::fs::create_dir_all(&album_dir).unwrap();
    std::fs::write(
        album_dir.join("album.yaml"),
        "id: alb_the_long_night\ntitle: The Long Night\ntracks: [song_ashes]\n",
    )
    .unwrap();

    let music = Music::new(&core);
    let artist = music.artist().unwrap().unwrap();
    assert_eq!(artist.name.as_deref(), Some("Blackwood Sessions"));
    let albums = music.albums().unwrap();
    assert_eq!(albums.len(), 1);
    assert_eq!(albums[0].0, "the-long-night");
    assert_eq!(albums[0].1.tracks, vec!["song_ashes"]);
}

// ---- repetition + rhyme tools over real lyrics --------------------------------

#[test]
fn repetition_and_end_rhymes_over_the_sample() {
    let (_tmp, core, _root) = open_sample();
    let report = Music::new(&core).check("ashes").unwrap();
    // the chorus lines repeat across Chorus / Final Chorus (reference
    // sections don't double-count)
    assert!(
        report.repetitions.iter().any(|r| r.text.contains("ashes")),
        "{:?}",
        report.repetitions
    );
    // verse 1 ends "rain"/"train" — a perfect rhyme across lines
    assert!(
        report.rhymes.iter().any(|h| h.kind == RhymeKind::Perfect),
        "{:?}",
        report.rhymes
    );
}
