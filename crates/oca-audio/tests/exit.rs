//! Phase 4 exit criteria: record → save → attach to song and to chapter →
//! play back — plus the v2 additions: Creative Note fields with reserved
//! melody data, checksum-recorded sidecars, and clean behaviour on
//! device-less machines. Devices themselves are covered by the manual
//! per-OS checklist (`docs/audio-device-checklist.md`); everything here
//! runs on the deterministic mock host.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use oca_audio::{
    Attachment, Audio, MockHost, Note, RecState, Recorder, TakeStore, default_take_dir,
};
use oca_core::{Command, Core};

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

fn audio<'c>(core: &'c Core, host: Arc<MockHost>) -> Audio<'c> {
    Audio::new(core, host).unwrap()
}

/// The exit-criteria pipeline, end to end, on the sample project.
#[test]
fn record_save_attach_play_round_trip() {
    let (_tmp, core, root) = open_sample();
    let host = Arc::new(MockHost::with_chunks(
        oca_audio::AudioSpec::mono(8_000),
        vec![vec![0.1; 400], vec![0.2; 400]],
    ));
    let facade = audio(&core, Arc::clone(&host));

    // record → pause → resume → stop
    let mut recorder = facade.record().unwrap();
    assert_eq!(recorder.state(), RecState::Recording);
    recorder.pause().unwrap();
    recorder.resume().unwrap();
    let done = recorder.stop();
    assert_eq!(done.samples.len(), 800, "both capture chunks landed");
    // save
    let take = facade
        .save_take(&done, &default_take_dir(), Some("Chorus idea 7"))
        .unwrap();
    assert_eq!(
        take.wav_rel,
        PathBuf::from("audio/chorus-idea-7.wav"),
        "human title becomes the filename"
    );
    assert!(take.meta.checksum.starts_with("sha256:"));
    assert!(take.meta.take_id.starts_with("take_"));
    assert!(root.join(&take.wav_rel).is_file());

    // attach to a chapter (resolved through the story engine, id stored)
    let target = facade.resolve_target("chapter:1").unwrap();
    let take = facade.attach(&take, &target).unwrap();
    assert_eq!(
        take.meta.attached,
        Some(Attachment::Chapter {
            id: "ch_001".into()
        })
    );

    // attach to the song (resolved through the music engine)
    let target = facade.resolve_target("song:ashes").unwrap();
    let take = facade.attach(&take, &target).unwrap();
    assert_eq!(
        take.meta.attached,
        Some(Attachment::Song {
            id: "song_ashes".into()
        })
    );

    // play back: the mock receives exactly what the file contains
    let player = facade.play(&take).unwrap();
    assert!(player.is_done());
    let played = host.played();
    assert_eq!(played.len(), 1);
    let stored = facade.store().load(&take).unwrap();
    assert_eq!(played[0].1, stored.samples);

    // detach clears the link, everything else survives
    let take = facade.detach(&take).unwrap();
    assert!(take.meta.attached.is_none());

    // delete removes both files
    facade.delete(&take).unwrap();
    assert!(facade.takes().unwrap().is_empty());
    assert!(!root.join("audio/chorus-idea-7.wav").exists());
    assert!(!root.join("audio/chorus-idea-7.yaml").exists());
}

#[test]
fn attach_targets_resolve_through_the_story_and_music_engines() {
    let (_tmp, core, _root) = open_sample();
    let facade = audio(&core, Arc::new(MockHost::new()));

    assert_eq!(
        facade.resolve_target("project").unwrap(),
        Attachment::Project
    );
    assert!(matches!(
        facade.resolve_target("chapter:Blackwood").unwrap(),
        Attachment::Chapter { .. }
    ));
    assert!(matches!(
        facade.resolve_target("song:ashes").unwrap(),
        Attachment::Song { .. }
    ));

    assert!(facade.resolve_target("chapter:no-such-thing").is_err());
    assert!(facade.resolve_target("song:no-such-thing").is_err());
    assert!(facade.resolve_target("nonsense").is_err());
    assert!(facade.resolve_target("alien:xyz").is_err());
}

#[test]
fn songs_without_ids_cannot_be_audio_targets() {
    let (_tmp, mut core, root) = open_sample();
    // Author a new song, then strip its id — links need a stable id.
    oca_music::Music::new_song(&mut core, "untitled-demo", "Untitled Demo").unwrap();
    let yaml_rel = root.join("music/songs/untitled-demo/song.yaml");
    let raw = std::fs::read_to_string(&yaml_rel).unwrap();
    let stripped: String = raw
        .lines()
        .filter(|l| !l.starts_with("id:"))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&yaml_rel, format!("{stripped}\n")).unwrap();

    let facade = audio(&core, Arc::new(MockHost::new()));
    let err = facade.resolve_target("song:untitled-demo").unwrap_err();
    assert!(err.to_string().contains("id:"), "{err}");
}

#[test]
fn read_only_projects_refuse_every_write_but_still_list() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("blackwood");
    copy_tree(Path::new(SAMPLE), &root);
    // A future format_version opens the project read-only (Phase 1 gate).
    let yaml = root.join("project.yaml");
    let raw = std::fs::read_to_string(&yaml).unwrap();
    std::fs::write(
        &yaml,
        raw.replace("format_version: '1'", "format_version: '99'"),
    )
    .unwrap();
    let mut core = Core::new();
    core.execute(Command::OpenProject { root: root.clone() })
        .unwrap();
    assert!(core.is_read_only());

    let facade = audio(&core, Arc::new(MockHost::new()));
    let err = facade.record().unwrap_err();
    assert!(err.to_string().contains("cannot record"), "{err}");
    assert!(err.to_string().contains("newer"), "{err}");

    let store = TakeStore::new(&root);
    let take = store
        .save(&default_take_dir(), &recorded(), Some("seed"))
        .unwrap();
    let audio_data = recorded();
    assert!(
        facade
            .save_take(&audio_data, &default_take_dir(), None)
            .is_err()
    );
    assert!(facade.rename(&take, "other").is_err());
    assert!(facade.attach(&take, &Attachment::Project).is_err());
    assert!(facade.detach(&take).is_err());
    assert!(facade.delete(&take).is_err());
    assert!(facade.update_note(&take, Some("x"), None, None).is_err());
    // Reading still works.
    assert_eq!(facade.takes().unwrap().len(), 1);
}

fn recorded() -> oca_audio::RecordedAudio {
    oca_audio::RecordedAudio {
        samples: vec![0.0; 100],
        spec: oca_audio::AudioSpec::mono(8_000),
    }
}

#[test]
fn creative_note_fields_round_trip_and_reserved_data_survives() {
    let (_tmp, core, root) = open_sample();
    let facade = audio(&core, Arc::new(MockHost::new()));
    let take = facade
        .save_take(&recorded(), &default_take_dir(), Some("idea"))
        .unwrap();

    // Hand-add future melody data, as a post-v1 version would.
    let sidecar = root.join(&take.sidecar_rel);
    let raw = std::fs::read_to_string(&sidecar).unwrap();
    std::fs::write(
        &sidecar,
        format!("{raw}note:\n  melody:\n    notes: [C4, G4]\n"),
    )
    .unwrap();

    let take = facade.take("idea").unwrap();
    let take = facade
        .update_note(
            &take,
            Some("needs to feel bigger on the last line"),
            Some("\"I thought I knew...\""),
            Some(92),
        )
        .unwrap();
    let note: Note = take.meta.note.clone().unwrap();
    assert_eq!(note.tempo_bpm, Some(92));
    assert!(note.lyrics.is_some());
    assert!(note.melody.is_some(), "reserved melody data must survive");

    // Editing just the tempo keeps the text.
    let take = facade.update_note(&take, None, None, Some(100)).unwrap();
    let note = take.meta.note.unwrap();
    assert_eq!(note.tempo_bpm, Some(100));
    assert_eq!(
        note.text.as_deref(),
        Some("needs to feel bigger on the last line")
    );
}

#[test]
fn future_sidecar_keys_and_comments_survive_facade_edits() {
    let (_tmp, core, root) = open_sample();
    let facade = audio(&core, Arc::new(MockHost::new()));
    let take = facade
        .save_take(&recorded(), &default_take_dir(), Some("seed"))
        .unwrap();
    let sidecar = root.join(&take.sidecar_rel);
    let raw = std::fs::read_to_string(&sidecar).unwrap();
    std::fs::write(
        &sidecar,
        format!("# recorded on the field recorder\n{raw}waveform_cache: [1, 2, 3]\n"),
    )
    .unwrap();

    let take = facade.take("seed").unwrap();
    facade.attach(&take, &Attachment::Project).unwrap();
    let raw = std::fs::read_to_string(&sidecar).unwrap();
    assert!(raw.contains("# recorded on the field recorder"));
    assert!(raw.contains("waveform_cache: [1, 2, 3]"));
}

#[test]
fn takes_never_appear_as_documents() {
    let (_tmp, core, root) = open_sample();
    let facade = audio(&core, Arc::new(MockHost::new()));
    facade
        .save_take(&recorded(), &default_take_dir(), Some("idea"))
        .unwrap();
    facade
        .save_take(
            &recorded(),
            Path::new("music/songs/ashes/demos"),
            Some("guitar"),
        )
        .unwrap();
    let takes = facade.takes().unwrap();
    assert_eq!(takes.len(), 2, "audio/ and song demos/ both list");
    // The document index (markdown only) is unaffected.
    assert!(
        core.documents()
            .iter()
            .all(|d| !d.rel.to_string_lossy().ends_with(".wav"))
    );
    let _ = root;
}

#[test]
fn mock_probe_reports_devices_and_deviceless_hosts_do_not() {
    let host = MockHost::new();
    let report = Audio::probe(&host);
    assert!(report.available());
    assert_eq!(report.input.unwrap(), oca_audio::AudioSpec::mono(8_000));
    assert_eq!(report.output.unwrap(), oca_audio::AudioSpec::mono(8_000));

    let bare = MockHost::without_devices();
    let report = Audio::probe(&bare);
    assert!(!report.available());
    assert!(
        report
            .input
            .unwrap_err()
            .to_string()
            .contains("no audio input device")
    );
    assert!(
        report
            .output
            .unwrap_err()
            .to_string()
            .contains("no audio output device")
    );

    // And the recorder refuses to start, with a clear message.
    let err = Recorder::start(&bare).unwrap_err();
    assert!(err.to_string().contains("no audio input device"));
}
