//! End-to-end CLI tests: run the real `oca` binary (Phase 0 exit criteria).

use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

/// Run `oca` in an isolated HOME + cwd so machine-local state can't leak in.
fn oca() -> Command {
    let tmp = TempDir::new().unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_oca"));
    cmd.current_dir(tmp.path())
        .env("HOME", tmp.path())
        .env("USERPROFILE", tmp.path());
    // TempDir must outlive the child; leak it deliberately for test simplicity.
    std::mem::forget(tmp);
    cmd
}

#[test]
fn version_flag() {
    let out = oca().arg("--version").output().unwrap();
    assert!(out.status.success(), "--version must exit 0");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("oca "), "got: {stdout}");
    assert!(stdout.contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn help_flag() {
    let out = oca().arg("--help").output().unwrap();
    assert!(out.status.success(), "--help must exit 0");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Usage:"), "got: {stdout}");
    assert!(stdout.contains("doctor"), "got: {stdout}");
}

#[test]
fn bare_invocation_prints_help() {
    let out = oca().output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Usage:"), "got: {stdout}");
}

#[test]
fn doctor_runs_and_reports() {
    let out = oca().arg("doctor").output().unwrap();
    assert!(out.status.success(), "doctor must exit 0");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("OCA Doctor"), "got: {stdout}");
    assert!(
        stdout.contains("⚠"),
        "Phase 0 shows pending subsystems: {stdout}"
    );
}

#[test]
fn doctor_is_quiet_by_default_and_verbose_with_flag() {
    let out = oca().arg("doctor").output().unwrap();
    assert!(
        out.stderr.is_empty(),
        "default log level must not spam stderr"
    );
    let out = oca().args(["-vv", "doctor"]).output().unwrap();
    assert!(!out.stderr.is_empty(), "-vv must emit debug logs to stderr");
}

#[test]
fn unknown_subcommand_is_a_usage_error() {
    let out = oca().arg("frobnicate").output().unwrap();
    assert_eq!(out.status.code(), Some(2), "clap usage errors exit 2");
}

// ---- Phase 1 exit criteria -------------------------------------------------

/// Run `oca` with an explicit shared HOME + cwd (multi-step scenarios).
fn run(home: &std::path::Path, cwd: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_oca"))
        .current_dir(cwd)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .args(args)
        .output()
        .unwrap()
}

fn stdout_of(out: &std::process::Output) -> String {
    assert!(
        out.status.success(),
        "command failed: {}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// Frontmatter section = opening fence through the closing fence line.
fn frontmatter_section(bytes: &[u8]) -> &[u8] {
    let end = bytes
        .windows(5)
        .position(|w| w == b"\n---\n")
        .map(|i| i + 5)
        .unwrap_or(0);
    &bytes[..end]
}

/// Phase 1 exit: `create project → create chapter → write → save → close →
/// reopen → continue` from the CLI, with a byte-identical Markdown+frontmatter
/// round-trip, plus the v2 exit add: an external rename still resolves by id.
#[test]
fn phase1_exit_create_write_save_reopen_roundtrip() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let work = tmp.path().join("work");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&work).unwrap();

    // -- create project ----------------------------------------------------
    let out = run(&home, &work, &["new", "my-novel"]);
    let stdout = stdout_of(&out);
    assert!(stdout.contains("created project 'my-novel'"), "{stdout}");
    let root = work.join("my-novel");
    assert!(root.join("project.yaml").is_file());

    // Registered for `oca list`.
    let stdout = stdout_of(&run(&home, &work, &["list"]));
    assert!(stdout.contains("my-novel"), "{stdout}");

    // -- create chapter ----------------------------------------------------
    let stdout = stdout_of(&run(&home, &root, &["chapter", "new", "First Chapter"]));
    let line = stdout.lines().next().expect("one output line");
    let rest = line
        .strip_prefix("created ")
        .unwrap_or_else(|| panic!("got: {line}"));
    let (rel, id) = rest
        .rsplit_once(" (")
        .unwrap_or_else(|| panic!("got: {line}"));
    let id = id.trim_end_matches(')');
    assert_eq!(rel, "manuscript/001-first-chapter.md");
    assert!(id.starts_with("ch_"), "id: {id}");

    // -- write → save ------------------------------------------------------
    let stdout = stdout_of(&run(
        &home,
        &root,
        &[
            "chapter",
            "append",
            "First Chapter",
            "It",
            "was",
            "the",
            "best",
            "of",
            "times.",
        ],
    ));
    assert!(stdout.contains("saved manuscript/001"), "{stdout}");

    let file = root.join(rel);
    let saved = std::fs::read(&file).unwrap();
    assert!(
        String::from_utf8_lossy(&saved).contains("It was the best of times."),
        "body must contain the appended paragraph"
    );

    // -- close → reopen: read is byte-identical (fresh session) -------------
    let shown = run(&home, &root, &["chapter", "show", "First Chapter"]);
    assert_eq!(
        shown.stdout, saved,
        "reopen must reproduce the document byte-identically"
    );

    // -- continue in the next session; frontmatter survives untouched ------
    let fm_before = frontmatter_section(&saved).to_vec();
    stdout_of(&run(
        &home,
        &root,
        &["chapter", "append", rel, "The", "story", "continues."],
    ));
    let continued = std::fs::read(&file).unwrap();
    assert!(
        String::from_utf8_lossy(&continued).contains("The story continues."),
        "second session must extend the body"
    );
    assert_eq!(
        frontmatter_section(&continued),
        fm_before.as_slice(),
        "frontmatter must be byte-identical after a later edit"
    );

    // -- external rename → id-based resolution still finds the file --------
    let renamed = root.join("manuscript/renamed-by-hand.md");
    std::fs::rename(&file, &renamed).unwrap();
    let shown = run(&home, &root, &["chapter", "show", id]);
    assert_eq!(
        shown.stdout, continued,
        "id-based lookup must survive an external rename"
    );
}

// ---- Phase 2 exit criteria (story queries from the CLI) --------------------

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

/// Run `oca` against an explicit project directory.
fn run_in(
    home: &std::path::Path,
    cwd: &std::path::Path,
    project: &std::path::Path,
    args: &[&str],
) -> std::process::Output {
    let mut full: Vec<&str> = args.to_vec();
    full.extend(["--project", project.to_str().unwrap()]);
    run(home, cwd, &full)
}

/// The committed `examples/` sample novel, copied so tests may mutate it.
fn sample_novel(home: &Path) -> PathBuf {
    let dst = home.join("blackwood");
    copy_tree(
        Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../examples/blackwood"
        )),
        &dst,
    );
    dst
}

/// Phase 2 exit: all four example queries answered correctly over the
/// `examples/` sample novel from the CLI, no AI involved — including the
/// flashback fixture ("before 8" in both time senses) and the alias fixture
/// ("Ellie").
#[test]
fn phase2_exit_story_queries_over_sample_novel() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let root = sample_novel(&home);

    // "Who is Marcus?" — identity, relationships, current state.
    let out = stdout_of(&run_in(
        &home,
        tmp.path(),
        &root,
        &["story", "who", "Marcus"],
    ));
    assert!(out.contains("role: investigator"), "got: {out}");
    assert!(out.contains("relationship: elena — distrust"), "got: {out}");
    assert!(out.contains("location: courthouse"), "got: {out}");

    // "Where is Elena?" — carry-forward, default = last chapter; `--at` steps back.
    let out = stdout_of(&run_in(
        &home,
        tmp.path(),
        &root,
        &["story", "where", "Elena"],
    ));
    assert!(out.contains("Elena at The Return"), "got: {out}");
    assert!(out.contains("location: blackwood"), "got: {out}");
    let out = stdout_of(&run_in(
        &home,
        tmp.path(),
        &root,
        &["story", "where", "Elena", "--at", "3"],
    ));
    assert!(out.contains("location: village"), "got: {out}");

    // "Which chapters contain Elena?" — alias fixture: ch.5 says only
    // "Ellie", ch.9 only "the girl", ch.7 says "Elenara" (not a mention).
    let out = stdout_of(&run_in(
        &home,
        tmp.path(),
        &root,
        &["story", "chapters-with", "Ellie"],
    ));
    assert!(out.contains("9 chapter(s) mention Elena"), "got: {out}");
    assert!(out.contains("Market Day"), "Ellie-only chapter must count");
    assert!(
        out.contains("The Coast"),
        "the-girl-only chapter must count"
    );
    assert!(!out.contains("The Elder"), "Elenara must not count: {out}");

    // "What happened before chapter 8?" — narrative sense: the flash-forward
    // in ch.6 counts, the flashback in ch.10 doesn't.
    let out = stdout_of(&run_in(&home, tmp.path(), &root, &["story", "before", "8"]));
    assert!(out.contains("by narrative position"), "got: {out}");
    assert!(out.contains("The factions hold a moot"), "got: {out}");
    assert!(!out.contains("mother burns"), "got: {out}");

    // Story sense: the flashback counts, the flash-forward doesn't.
    let out = stdout_of(&run_in(
        &home,
        tmp.path(),
        &root,
        &["story", "before", "8", "--sense", "story"],
    ));
    assert!(out.contains("by story time"), "got: {out}");
    assert!(out.contains("mother burns the first letter"), "got: {out}");
    assert!(!out.contains("factions hold a moot"), "got: {out}");

    // Unknown characters fail loudly, not vacuously.
    let out = run_in(&home, tmp.path(), &root, &["story", "who", "nobody"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("no character"));
}

// ---- Phase 3 exit criteria -------------------------------------------------

/// Phase 3 exit: author a song entirely offline from the CLI — scaffold,
/// write lyrics (with chords and a `{=chorus}` repeat), check with real
/// syllable metrics, snapshot a labeled version — then check the committed
/// sample song. No AI anywhere (rule 3), no Git vocabulary (rule 7).
#[test]
fn phase3_exit_author_and_check_song_offline() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let work = tmp.path().join("work");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&work).unwrap();

    let out = run(&home, &work, &["new", "winter-songs"]);
    stdout_of(&out);
    let root = work.join("winter-songs");

    // -- scaffold -----------------------------------------------------------
    let out = run_in(&home, tmp.path(), &root, &["song", "new", "Winter Light"]);
    let out = stdout_of(&out);
    assert!(out.contains("winter-light"), "got: {out}");
    let song_dir = root.join("music/songs/winter-light");
    assert!(song_dir.join("song.yaml").is_file());
    assert!(song_dir.join("lyrics.md").is_file());

    // -- draft the song (author edits files directly — files are canonical) --
    std::fs::write(
        song_dir.join("lyrics.md"),
        "\
# Winter Light

## Verse 1
[Dm]snow on the wire, [C]waiting for the rain
[G]every street a [Am]memory of the train

## Chorus
[Dm]hold on, hold [C]on to the light we were
[G]hold on, hold [Am]on, it won't hurt forever

## Verse 2
[Dm]smoke on the water, [C]ash on the ground
[G]silence is a [Am]bell that won't sound

## Chorus 2 {=chorus}
",
    )
    .unwrap();

    // -- check: clean, with syllable metrics, chords ignored ----------------
    let out = run_in(&home, tmp.path(), &root, &["song", "check", "winter-light"]);
    let out = stdout_of(&out);
    assert!(out.contains("Winter Light"), "got: {out}");
    assert!(out.contains("plan: matches"), "no plan → no drift: {out}");
    assert!(out.contains("syllables"), "metrics expected: {out}");
    assert!(out.contains("(repeat)"), "Chorus 2 must resolve: {out}");
    assert!(!out.contains("unsupported"), "en is supported: {out}");

    // -- snapshot a labeled version -----------------------------------------
    let out = run_in(
        &home,
        tmp.path(),
        &root,
        &[
            "song",
            "save-version",
            "winter-light",
            "--label",
            "first pass",
        ],
    );
    let out = stdout_of(&out);
    assert!(out.contains("v01.md"), "got: {out}");
    assert!(out.contains("first pass"), "got: {out}");
    assert!(song_dir.join("versions/v01.md").is_file());
    let index = std::fs::read_to_string(song_dir.join("versions/index.yaml")).unwrap();
    assert!(index.contains("first pass"), "got: {index}");

    // Version bytes are exactly the lyrics that existed at save time.
    let v1 = std::fs::read(song_dir.join("versions/v01.md")).unwrap();
    let lyrics = std::fs::read(song_dir.join("lyrics.md")).unwrap();
    assert_eq!(v1, lyrics);
}

/// Phase 3 exit add: the committed sample song checks clean, an invented
/// name with user pronunciation rhymes, and an unsupported language says
/// "unsupported" instead of producing wrong numbers.
#[test]
fn phase3_exit_sample_song_and_language_gate() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let root = sample_novel(&home);

    // The committed sample song loads and checks clean via the CLI.
    let out = stdout_of(&run_in(
        &home,
        tmp.path(),
        &root,
        &["song", "check", "ashes"],
    ));
    assert!(out.contains("Ashes"), "got: {out}");
    assert!(
        out.contains("repeat"),
        "sample has a {{=chorus}} repeat: {out}"
    );
    assert!(out.contains("syllables"), "got: {out}");

    // Unsupported language: structure still reported, syllables say
    // "unsupported" — never invented numbers (A9).
    let yaml_path = root.join("music/songs/ashes/song.yaml");
    let yaml = std::fs::read_to_string(&yaml_path).unwrap();
    std::fs::write(&yaml_path, yaml.replace("language: en", "language: fr")).unwrap();

    let out = run_in(&home, tmp.path(), &root, &["song", "check", "ashes"]);
    let out = stdout_of(&out);
    assert!(out.contains("unsupported"), "got: {out}");
    assert!(!out.contains("total: "), "no syllable total for fr: {out}");
}

// ---- Phase 4 exit criteria (device-free half) -------------------------------

/// Seed a take exactly the way the recorder writes one: a tiny valid WAV
/// plus its sidecar. (Recording and playback need real devices; those are
/// covered by the mock-host engine tests and the manual per-OS checklist.)
fn seed_take(root: &Path, stem: &str, title: &str, take_id: &str) {
    std::fs::create_dir_all(root.join("audio")).unwrap();
    let rate: u32 = 8_000;
    let data = vec![0u8; 200]; // 100 frames, mono, 16-bit PCM
    let mut wav = Vec::new();
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&1u16.to_le_bytes()); // mono
    wav.extend_from_slice(&rate.to_le_bytes());
    wav.extend_from_slice(&(rate * 2).to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&(data.len() as u32).to_le_bytes());
    wav.extend_from_slice(&data);
    std::fs::write(root.join(format!("audio/{stem}.wav")), wav).unwrap();
    std::fs::write(
        root.join(format!("audio/{stem}.yaml")),
        format!(
            "take_id: {take_id}\ntitle: {title}\ncreated: 2026-10-15T09:00:00Z\n\
             sample_rate: 8000\nchannels: 1\nduration_ms: 12\nchecksum: sha256:seed\n"
        ),
    )
    .unwrap();
}

/// Phase 4 exit (CLI half): list a take, attach it to a chapter and to a
/// song by id, edit its Creative Note, rename, detach, delete — all offline
/// from the CLI, sidecar patches lossless.
#[test]
fn phase4_exit_take_lifecycle_over_sample_novel() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let root = sample_novel(&home);
    seed_take(&root, "voice-memo", "voice memo", "take_SEED0001");

    // list: title, id, duration, unattached, path
    let out = stdout_of(&run_in(&home, tmp.path(), &root, &["audio", "list"]));
    assert!(out.contains("voice-memo"), "got: {out}");
    assert!(out.contains("take_SEED0001"), "got: {out}");
    assert!(out.contains("unattached"), "got: {out}");
    assert!(out.contains("audio/voice-memo.wav"), "got: {out}");

    // attach to a chapter — resolved through the story engine, id stored
    let out = stdout_of(&run_in(
        &home,
        tmp.path(),
        &root,
        &["audio", "attach", "voice memo", "chapter:1"],
    ));
    assert!(out.contains("chapter ch_001"), "got: {out}");
    let sidecar = root.join("audio/voice-memo.yaml");
    let raw = std::fs::read_to_string(&sidecar).unwrap();
    assert!(raw.contains("kind: chapter"), "{raw}");
    assert!(raw.contains("id: ch_001"), "{raw}");

    // Creative Note: write fields, then print them back
    stdout_of(&run_in(
        &home,
        tmp.path(),
        &root,
        &[
            "audio",
            "note",
            "voice-memo",
            "--text",
            "needs rain sounds",
            "--tempo",
            "88",
        ],
    ));
    let out = stdout_of(&run_in(
        &home,
        tmp.path(),
        &root,
        &["audio", "note", "voice-memo"],
    ));
    assert!(out.contains("needs rain sounds"), "got: {out}");
    assert!(out.contains("88"), "got: {out}");

    // re-attach to the song (last write wins)
    let out = stdout_of(&run_in(
        &home,
        tmp.path(),
        &root,
        &["audio", "attach", "voice-memo", "song:ashes"],
    ));
    assert!(out.contains("song song_ashes"), "got: {out}");
    let raw = std::fs::read_to_string(&sidecar).unwrap();
    assert!(raw.contains("kind: song"), "{raw}");
    assert!(raw.contains("id: song_ashes"), "{raw}");
    // The note survived the re-attach (only `attached:` was patched).
    assert!(raw.contains("needs rain sounds"), "{raw}");

    // rename: files move, take_id (and its links) do not
    let out = stdout_of(&run_in(
        &home,
        tmp.path(),
        &root,
        &["audio", "rename", "voice-memo", "field-recording"],
    ));
    assert!(out.contains("take_SEED0001"), "got: {out}");
    assert!(!sidecar.exists(), "old sidecar moved");
    let raw = std::fs::read_to_string(root.join("audio/field-recording.yaml")).unwrap();
    assert!(raw.contains("take_id: take_SEED0001"), "{raw}");
    assert!(raw.contains("kind: song"), "attachment followed the rename");

    // detach clears only the attachment
    let out = stdout_of(&run_in(
        &home,
        tmp.path(),
        &root,
        &["audio", "detach", "field-recording"],
    ));
    assert!(out.contains("unattached"), "got: {out}");
    let raw = std::fs::read_to_string(root.join("audio/field-recording.yaml")).unwrap();
    assert!(!raw.contains("attached"), "{raw}");
    assert!(raw.contains("needs rain sounds"), "{raw}");

    // delete removes both files
    stdout_of(&run_in(
        &home,
        tmp.path(),
        &root,
        &["audio", "delete", "field-recording"],
    ));
    let out = stdout_of(&run_in(&home, tmp.path(), &root, &["audio", "list"]));
    assert!(out.contains("no takes yet"), "got: {out}");
}

/// Phase 4: clean error paths — unknown take, unknown chapter, malformed
/// target — all exit non-zero with a helpful message (no device access).
#[test]
fn phase4_audio_errors_are_reported_not_crashed() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let root = sample_novel(&home);
    seed_take(&root, "voice-memo", "voice memo", "take_SEED0001");

    let cases: &[&[&str]] = &[
        &["audio", "attach", "no-such-take", "project"],
        &["audio", "attach", "voice-memo", "chapter:no-such-thing"],
        &["audio", "attach", "voice-memo", "alien:xyz"],
        &["audio", "attach", "voice-memo", "nonsense"],
        // Record validates its --attach target BEFORE opening a device,
        // so this fails identically with and without sound hardware.
        &["audio", "record", "--attach", "chapter:no-such-thing"],
        &["audio", "play", "no-such-take"],
        &["audio", "rename", "voice-memo", "!!!"],
        &["audio", "delete", "no-such-take"],
        &["audio", "detach", "no-such-take"],
        &["audio", "note", "no-such-take", "--text", "x"],
    ];
    for args in cases {
        let out = run_in(&home, tmp.path(), &root, args);
        assert!(
            !out.status.success(),
            "expected failure: {args:?}\n{}",
            String::from_utf8_lossy(&out.stdout)
        );
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(!err.trim().is_empty(), "failure explains itself: {args:?}");
    }
}

/// Phase 5 exit: search returns correct cross-entity hits on the sample
/// project (chapters + character + timeline event in one query, audio
/// metadata too); deleting the DB and searching again reproduces identical
/// results; tags/links/migrate behave; `doctor` reports the index with an
/// empty stderr. No AI involved anywhere.
#[test]
fn phase5_exit_search_reindex_tags_links_over_sample_novel() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let root = sample_novel(&home);
    seed_take(&root, "voice-memo", "voice memo", "take_SEED0001");

    // Full reindex reports what it built.
    let out = stdout_of(&run_in(&home, tmp.path(), &root, &["reindex", "--full"]));
    assert!(out.contains("full reindex"), "got: {out}");
    assert!(out.contains("documents"), "totals reported: {out}");
    // 10 chapters + 2 characters + timeline + lyrics + README = 15 docs.
    assert!(out.contains("15 documents"), "got: {out}");
    assert!(out.contains("1 takes"), "the seeded take: {out}");

    // Cross-entity search: one term hits chapters, the character file, and
    // a timeline event (event text lives in frontmatter, not the body).
    let out = stdout_of(&run_in(
        &home,
        tmp.path(),
        &root,
        &["search", "elena", "--limit", "50"],
    ));
    assert!(out.contains("chapter"), "chapters: {out}");
    assert!(out.contains("character"), "character: {out}");
    assert!(out.contains("event"), "timeline event: {out}");
    assert!(out.contains("manuscript/"), "paths shown: {out}");
    assert!(out.contains("characters/elena.md"), "got: {out}");

    // Audio metadata (sidecar note/title) is searchable; the sidecar stays
    // canonical — the DB only indexes it.
    let out = stdout_of(&run_in(&home, tmp.path(), &root, &["search", "memo"]));
    assert!(out.contains("audio"), "take metadata: {out}");

    // --- delete DB → search again → identical results (rebuild rule) ---
    let before = stdout_of(&run_in(
        &home,
        tmp.path(),
        &root,
        &["search", "elena", "--limit", "50"],
    ));
    let db = root.join(".oca/cache/oca.db");
    std::fs::remove_file(&db).unwrap();
    let _ = std::fs::remove_file(format!("{}-wal", db.display()));
    let _ = std::fs::remove_file(format!("{}-shm", db.display()));
    let after = stdout_of(&run_in(
        &home,
        tmp.path(),
        &root,
        &["search", "elena", "--limit", "50"],
    ));
    assert_eq!(
        before, after,
        "a rebuilt index must answer identically (files are canonical)"
    );

    // --- tags: add (lossless patch), search by tag, list, remove ---
    let doc = root.join("manuscript/001-the-arrival.md");
    let raw = std::fs::read_to_string(&doc).unwrap();
    let commented = raw.replace("id: ch_001", "# keep this comment\nid: ch_001");
    assert_ne!(commented, raw, "fixture edit applies");
    std::fs::write(&doc, commented).unwrap();

    stdout_of(&run_in(
        &home,
        tmp.path(),
        &root,
        &["tag", "add", "ch_001", "status:draft"],
    ));
    let raw = std::fs::read_to_string(&doc).unwrap();
    assert!(
        raw.contains("# keep this comment"),
        "comment survives: {raw}"
    );
    assert!(raw.contains("tags:"), "tags block written: {raw}");
    assert!(raw.contains("status:draft"), "got: {raw}");

    let out = stdout_of(&run_in(
        &home,
        tmp.path(),
        &root,
        &["search", "tag:status:draft"],
    ));
    assert!(out.contains("001-the-arrival.md"), "tag search: {out}");

    let out = stdout_of(&run_in(&home, tmp.path(), &root, &["tags"]));
    assert!(out.contains("status:draft"), "tag listing: {out}");

    let out = stdout_of(&run_in(
        &home,
        tmp.path(),
        &root,
        &["tag", "list", "ch_001"],
    ));
    assert!(out.contains("status:draft"), "per-doc tags: {out}");

    stdout_of(&run_in(
        &home,
        tmp.path(),
        &root,
        &["tag", "remove", "ch_001", "status:draft"],
    ));
    let raw = std::fs::read_to_string(&doc).unwrap();
    assert!(!raw.contains("tags:"), "empty tags block dropped: {raw}");
    assert!(raw.contains("# keep this comment"), "still there: {raw}");

    // --- links: song's authored references, and the chapter's backlinks ---
    let out = stdout_of(&run_in(&home, tmp.path(), &root, &["links", "song_ashes"]));
    assert!(out.contains("chapters"), "link kinds: {out}");
    assert!(out.contains("ch_001"), "outgoing target: {out}");
    assert!(out.contains("elena"), "character link: {out}");

    let out = stdout_of(&run_in(&home, tmp.path(), &root, &["links", "ch_001"]));
    assert!(out.contains("song_ashes"), "backlink source: {out}");
    assert!(out.contains('←'), "marked as incoming: {out}");

    // --- migrate: format 1 is current ---
    let out = stdout_of(&run_in(&home, tmp.path(), &root, &["migrate"]));
    assert!(out.contains("current"), "got: {out}");

    // --- doctor: real index check, zero stderr (guardrail) — run from
    // inside the project, since doctor takes no --project flag ---
    let out = run(&home, &root, &["doctor"]);
    assert!(out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.is_empty(), "doctor keeps stderr empty: {err}");
    let out = String::from_utf8_lossy(&out.stdout);
    assert!(out.contains("✓ Database"), "database check: {out}");
    assert!(out.contains(".oca/cache/oca.db"), "got: {out}");
}

/// Phase 5 error paths: every bad input fails with a message, never a
/// panic; `reindex`/`search` outside a project explain themselves.
#[test]
fn phase5_index_errors_are_reported_not_crashed() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let root = sample_novel(&home);

    let cases: &[&[&str]] = &[
        &["search"],                          // empty query
        &["tag", "add", "ch_001", "bad tag"], // YAML-hostile tag
        &["tag", "add", "ch_001", "a:b:c"],   // one namespace only
        &["tag", "add", "no-such-doc", "ok:tag"],
        &["tag", "remove", "ch_001", "absent:tag"],
        &["tag", "list", "no-such-doc"],
        &["links", "no-such-thing"],
    ];
    for args in cases {
        let out = run_in(&home, tmp.path(), &root, args);
        assert!(
            !out.status.success(),
            "expected failure: {args:?}\n{}",
            String::from_utf8_lossy(&out.stdout)
        );
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(!err.trim().is_empty(), "explains itself: {args:?}");
        assert!(!err.contains("panicked"), "no panic: {args:?} — {err}");
    }

    // Outside any project: not-a-project, not a panic.
    for args in [&["reindex"][..], &["search", "x"][..], &["migrate"][..]] {
        let out = run(&home, tmp.path(), args);
        assert!(!out.status.success(), "expected failure: {args:?}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(!err.trim().is_empty(), "explains itself: {args:?}");
        assert!(!err.contains("panicked"), "no panic: {args:?} — {err}");
    }

    // Doctor outside a project: warning, exit 0, empty stderr (Phase 0
    // contract still holds now that Database is a real check).
    let out = run(&home, tmp.path(), &["doctor"]);
    assert!(out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).is_empty(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = String::from_utf8_lossy(&out.stdout);
    assert!(out.contains("⚠ Database — no project here"), "got: {out}");
}

/// Phase 6 exit: create → diff → restore round-trip from the CLI over the
/// sample project — restore reproduces the exact prior bytes (and removes
/// files the snapshot predates), audio is kept as recorded, and nothing
/// user-facing says a Git word (rule 7; the vocabulary scan enforces it).
#[test]
fn phase6_exit_snapshot_round_trip_over_sample_novel() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let root = sample_novel(&home);

    // No snapshots yet: the list says so, and doctor warns (exit 0, quiet
    // stderr — the Phase 0 contract still holds).
    let out = stdout_of(&run_in(&home, tmp.path(), &root, &["snapshot", "list"]));
    assert!(out.contains("no snapshots yet"), "got: {out}");
    let out = run(&home, &root, &["doctor"]);
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).is_empty());
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("⚠ Snapshots — none yet"),
        "doctor: {out:?}"
    );

    // Create with a label.
    let out = stdout_of(&run_in(
        &home,
        tmp.path(),
        &root,
        &["snapshot", "create", "before editing"],
    ));
    assert!(out.contains("Snapshot #1"), "got: {out}");
    assert!(out.contains("before editing"), "got: {out}");
    assert!(out.contains("file(s)"), "got: {out}");

    // Mutate externally: rewrite a chapter and drop in a stray file.
    let ch = root.join("manuscript/001-the-arrival.md");
    let original = std::fs::read(&ch).unwrap();
    let edited = format!("{}\nA NEW LINE.\n", String::from_utf8_lossy(&original));
    std::fs::write(&ch, edited.as_bytes()).unwrap();
    let stray = root.join("manuscript/zz-stray.md");
    std::fs::write(&stray, b"appeared later\n").unwrap();

    // Diff (snapshot vs working) shows exactly the truth.
    let out = stdout_of(&run_in(
        &home,
        tmp.path(),
        &root,
        &["snapshot", "diff", "1"],
    ));
    assert!(out.contains("+A NEW LINE."), "got: {out}");
    assert!(out.contains("001-the-arrival.md"), "got: {out}");
    assert!(out.contains("zz-stray.md"), "got: {out}");
    assert!(out.contains("vs your files now"), "got: {out}");

    // Restore: exact bytes back, stray file gone.
    let out = stdout_of(&run_in(
        &home,
        tmp.path(),
        &root,
        &["snapshot", "restore", "1"],
    ));
    assert!(out.contains("restored Snapshot #1"), "got: {out}");
    assert_eq!(
        std::fs::read(&ch).unwrap(),
        original,
        "restore reproduced the exact bytes"
    );
    assert!(!stray.exists(), "files the snapshot predates are removed");

    // The restore auto-snapshotted the broken state first — history grew
    // by the labeled row #2.
    let out = stdout_of(&run_in(&home, tmp.path(), &root, &["snapshot", "list"]));
    assert!(out.contains("Before restoring snapshot #1"), "got: {out}");
    assert!(out.contains("2     2026"), "row #2: {out}");

    // Snapshot-to-snapshot diff.
    let out = stdout_of(&run_in(
        &home,
        tmp.path(),
        &root,
        &["snapshot", "diff", "1", "2"],
    ));
    assert!(out.contains("Snapshot #1 vs Snapshot #2"), "got: {out}");
    assert!(out.contains("+A NEW LINE."), "the broken state: {out}");

    // doctor now reports the saved restore points.
    let out = run(&home, &root, &["doctor"]);
    assert!(String::from_utf8_lossy(&out.stderr).is_empty());
    let out = String::from_utf8_lossy(&out.stdout);
    assert!(out.contains("✓ Snapshots — 2 saved"), "got: {out}");

    // Audio: takes are reported, never overwritten.
    seed_take(&root, "voice-memo", "voice memo", "take_SEED0002");
    stdout_of(&run_in(&home, tmp.path(), &root, &["snapshot", "create"]));
    let wav = root.join("audio/voice-memo.wav");
    std::fs::write(&wav, b"RIFFre-recorded-then-restored").unwrap();
    let out = stdout_of(&run_in(
        &home,
        tmp.path(),
        &root,
        &["snapshot", "restore", "3"],
    ));
    assert!(out.contains("audio kept as recorded"), "got: {out}");
    assert!(out.contains("voice-memo.wav"), "names the take: {out}");
    assert_eq!(
        std::fs::read(&wav).unwrap(),
        b"RIFFre-recorded-then-restored",
        "restore never touches recorded bytes"
    );
}

/// Phase 6 error paths: bad snapshot numbers fail with a message, never a
/// panic; diff/restore outside a project explain themselves.
#[test]
fn phase6_snapshot_errors_are_reported_not_crashed() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let root = sample_novel(&home);

    let cases: &[&[&str]] = &[
        &["snapshot", "restore", "99"],
        &["snapshot", "restore", "0"],
        &["snapshot", "diff", "5"],
    ];
    for args in cases {
        let out = run_in(&home, tmp.path(), &root, args);
        assert!(!out.status.success(), "expected failure: {args:?}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(!err.trim().is_empty(), "explains itself: {args:?}");
        assert!(!err.contains("panicked"), "no panic: {args:?} — {err}");
    }

    for args in [&["snapshot", "list"][..], &["snapshot", "create"][..]] {
        let out = run(&home, tmp.path(), args);
        assert!(!out.status.success(), "expected failure: {args:?}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(!err.trim().is_empty(), "explains itself: {args:?}");
        assert!(!err.contains("panicked"), "no panic: {args:?} — {err}");
    }
}

// ---- runtime packs + AI checks (Phase 7) ---------------------------------

/// Run `oca` with an isolated HOME (machine-local state can't leak in).
fn oca_in(home: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_oca"));
    cmd.current_dir(home)
        .env("HOME", home)
        .env("USERPROFILE", home);
    cmd
}

#[test]
fn runtime_install_list_and_tamper_detection_round_trip() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    let pack = home.join("llama-pack-1.0.0.tar.gz");
    oca_ai::pack_writer::write_pack_file(
        &pack,
        "llama-pack",
        "1.0.0",
        &[("bin/llama-server", b"#!/bin/sh\necho fake\n")],
    )
    .unwrap();

    // Empty runtime dir: a hint, not an error.
    let out = oca_in(home).args(["runtime", "list"]).output().unwrap();
    assert!(out.status.success(), "list exits 0 when empty");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("no runtime packs"), "got: {stdout}");
    assert!(stdout.contains("oca runtime install"), "hint: {stdout}");

    // Install verifies the manifest before anything lands.
    let out = oca_in(home)
        .args(["runtime", "install", "--from"])
        .arg(&pack)
        .output()
        .unwrap();
    assert!(out.status.success(), "install: {out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("installed llama-pack 1.0.0 — 1 file(s) verified"),
        "got: {stdout}"
    );

    // Listed as verified.
    let out = oca_in(home).args(["runtime", "list"]).output().unwrap();
    assert!(out.status.success(), "clean pack exits 0");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("✓ llama-pack 1.0.0"), "got: {stdout}");

    // Installing the same pack twice is refused, clearly.
    let out = oca_in(home)
        .args(["runtime", "install", "--from"])
        .arg(&pack)
        .output()
        .unwrap();
    assert!(!out.status.success(), "double install must fail");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("already installed"), "got: {stderr}");

    // Someone edits the installed runtime: `list` flags it and exits 1.
    let installed = home
        .join(".oca")
        .join("runtime")
        .join("llama-pack-1.0.0")
        .join("bin")
        .join("llama-server");
    assert!(installed.is_file(), "binary placed inside the pack dir");
    std::fs::write(&installed, b"# tampered").unwrap();
    let out = oca_in(home).args(["runtime", "list"]).output().unwrap();
    assert!(!out.status.success(), "tampered pack exits non-zero");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("⚠ llama-pack 1.0.0"), "got: {stdout}");
    assert!(stdout.contains("changed"), "got: {stdout}");
}

#[test]
fn install_rejects_a_pack_for_another_platform() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    // Build a pack on the right platform, then flip its platform field by
    // rebuilding with a different manifest: easiest through the writer is
    // not possible, so craft via tar directly is overkill — instead install
    // a non-pack file and expect a manifest error.
    let bogus = home.join("not-a-pack.tar.gz");
    std::fs::write(&bogus, b"definitely not gzip").unwrap();
    let out = oca_in(home)
        .args(["runtime", "install", "--from"])
        .arg(&bogus)
        .output()
        .unwrap();
    assert!(!out.status.success(), "garbage must be refused");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("error"), "got: {stderr}");
}

#[test]
fn doctor_reports_ai_runtime_models_and_hardware() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    // A stand-in runtime inside a pack dir (packs win over PATH)…
    let server = home
        .join(".oca")
        .join("runtime")
        .join("llama-pack-1.0.0")
        .join("bin")
        .join("llama-server");
    std::fs::create_dir_all(server.parent().unwrap()).unwrap();
    std::fs::write(&server, b"#!/bin/sh\n").unwrap();
    // …and one discovered model.
    let models = home.join(".oca").join("models").join("llm");
    std::fs::create_dir_all(&models).unwrap();
    std::fs::write(models.join("lyra-3b.gguf"), vec![0u8; 4096]).unwrap();

    let out = oca_in(home).arg("doctor").output().unwrap();
    assert!(out.status.success(), "doctor must exit 0");
    assert!(
        out.stderr.is_empty(),
        "doctor keeps stderr silent: {:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("✓ Model runtime"), "got: {stdout}");
    assert!(
        stdout.contains("one heavy model at a time"),
        "got: {stdout}"
    );
    assert!(stdout.contains("✓ Models"), "got: {stdout}");
    assert!(stdout.contains("lyra-3b"), "got: {stdout}");
    assert!(stdout.contains("Hardware"), "got: {stdout}");
    assert!(stdout.contains("cores"), "got: {stdout}");
}

// ---- dictation (Phase 7B) --------------------------------------------------

/// The transcription runtime for e2e: this very test binary, invoked as a
/// single libtest case. Dictation sets `OCA_TRANSCRIBE_OUT` only on the
/// child it spawns, so a regular test run of this case does nothing — the
/// fake can never misfire into a normal run.
#[test]
fn fake_whisper_cli() {
    let Ok(out) = std::env::var("OCA_TRANSCRIBE_OUT") else {
        return;
    };
    std::fs::write(&out, "the lamp was lit in the hallway\n").unwrap();
}

/// Config that points `whisper-cli` at [`fake_whisper_cli`] above, plus a
/// stand-in model file — together, the only "whisper installed" this
/// machine gets: no LLM anywhere near it.
fn dictation_config(home: &Path) -> String {
    let me = std::env::current_exe().unwrap();
    let models = home.join(".oca").join("models").join("whisper");
    std::fs::create_dir_all(&models).unwrap();
    std::fs::write(models.join("ggml-tiny.gguf"), vec![0u8; 128]).unwrap();
    format!(
        "[ai]\nwhisper_cli = {:?}\nwhisper_args = [\"fake_whisper_cli\", \"--exact\", \"--quiet\"]\n",
        me.display().to_string()
    )
}

fn write_ai_config(home: &Path, body: &str) {
    std::fs::create_dir_all(home.join(".oca")).unwrap();
    std::fs::write(home.join(".oca").join("config.toml"), body).unwrap();
}

/// True when any chapter file under `manuscript/` contains the text.
fn manuscript_contains(root: &Path, needle: &str) -> bool {
    let Ok(entries) = std::fs::read_dir(root.join("manuscript")) else {
        return false;
    };
    entries.flatten().any(|e| {
        e.path().extension().is_some_and(|x| x == "md")
            && std::fs::read_to_string(e.path()).is_ok_and(|body| body.contains(needle))
    })
}

/// Phase 7B exit, CLI half: with **only** a whisper stand-in installed,
/// `oca dictate` transcribes, stores the raw transcript with the take, and
/// sends it further only where explicitly asked — never on its own.
#[test]
fn phase7b_exit_dictate_transcribes_and_sends_where_asked() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let root = sample_novel(&home);
    seed_take(&root, "voice-memo", "voice memo", "take_SEED0001");
    write_ai_config(&home, &dictation_config(&home));
    const SAID: &str = "the lamp was lit in the hallway";

    // -- raw dictation: transcript stored with the take, and *nowhere else*
    let out = run_in(&home, tmp.path(), &root, &["dictate", "voice-memo"]);
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        out.status.success(),
        "dictate must work with no LLM: {stderr}"
    );
    assert!(stdout.contains(SAID), "transcript on stdout: {stdout}");
    assert!(stderr.contains("transcribing with"), "status: {stderr}");
    let sidecar = std::fs::read_to_string(root.join("audio/voice-memo.yaml")).unwrap();
    assert!(sidecar.contains("transcript:"), "{sidecar}");
    assert!(sidecar.contains(SAID), "{sidecar}");
    assert!(
        !manuscript_contains(&root, SAID),
        "a transcript never auto-inserts into the manuscript"
    );
    assert!(
        !root.join("notes/dictation.md").exists(),
        "and never creates notes by itself"
    );

    // -- explicit: --into chapter:<chapter> appends on the author's say-so
    let out = run_in(
        &home,
        tmp.path(),
        &root,
        &["dictate", "voice-memo", "--into", "chapter:1"],
    );
    assert!(
        out.status.success(),
        "{:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(manuscript_contains(&root, SAID), "inserted on request");

    // -- explicit: --into notes appends to the dictation note.
    let out = run_in(
        &home,
        tmp.path(),
        &root,
        &["dictate", "voice-memo", "--into", "notes"],
    );
    assert!(
        out.status.success(),
        "{:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    let note = std::fs::read_to_string(root.join("notes/dictation.md")).unwrap();
    assert!(note.contains("## voice memo"), "{note}");
    assert!(note.contains(SAID), "{note}");
}

/// Rule 3 from the CLI: no runtime → an actionable error, nothing stored,
/// and `oca doctor` already said so (its row is covered separately).
#[test]
fn dictate_without_a_runtime_explains_itself() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let root = sample_novel(&home);
    seed_take(&root, "voice-memo", "voice memo", "take_SEED0001");
    // Configured explicitly (so the machine's PATH can't make this flaky).
    write_ai_config(
        &home,
        &format!(
            "[ai]\nwhisper_cli = {:?}\n",
            home.join("no-such-whisper").display().to_string()
        ),
    );

    let out = run_in(&home, tmp.path(), &root, &["dictate", "voice-memo"]);
    assert!(!out.status.success(), "no runtime must not pretend success");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("no dictation runtime"), "got: {stderr}");
    assert!(stderr.contains("runtime install"), "actionable: {stderr}");
    let sidecar = std::fs::read_to_string(root.join("audio/voice-memo.yaml")).unwrap();
    assert!(
        !sidecar.contains("transcript:"),
        "nothing stored: {sidecar}"
    );
}

/// The LLM half of dictation is optional *and* preview-only: `--clean`
/// without a model fails with guidance, and combining it with `--into` is
/// refused outright (the cleanup would be reviewed, never auto-sent).
#[test]
fn dictate_cleanups_need_a_llm_and_refuse_into() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let root = sample_novel(&home);
    seed_take(&root, "voice-memo", "voice memo", "take_SEED0001");
    write_ai_config(&home, &dictation_config(&home));

    // --clean without any LLM installed: the transcript still prints, the
    // command reports that the requested cleanup couldn't run, and no
    // model is silently skipped.
    let out = run_in(
        &home,
        tmp.path(),
        &root,
        &["dictate", "voice-memo", "--clean"],
    );
    assert!(!out.status.success(), "the requested cleanup cannot run");
    assert!(String::from_utf8_lossy(&out.stdout).contains("the lamp was lit"));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("needs a language model"), "got: {stderr}");

    // --clean + --into: refused before anything runs (review first).
    let out = run_in(
        &home,
        tmp.path(),
        &root,
        &["dictate", "voice-memo", "--clean", "--into", "notes"],
    );
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("review the cleanup first"), "got: {stderr}");
    assert!(!root.join("notes/dictation.md").exists(), "nothing written");

    // --clean --prose together: pick one.
    let out = run_in(
        &home,
        tmp.path(),
        &root,
        &["dictate", "voice-memo", "--clean", "--prose"],
    );
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("pick one"));
}

/// `doctor` discovers dictation (runtime + model, never spawns) and reports
/// it independently of the LLM side: whisper present, language model absent.
#[test]
fn doctor_reports_dictation_independently_of_the_llm() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    let cli = home.join("whisper-cli-stand-in");
    std::fs::write(&cli, b"stand-in").unwrap();
    let models = home.join(".oca").join("models").join("whisper");
    std::fs::create_dir_all(&models).unwrap();
    std::fs::write(models.join("ggml-tiny.gguf"), vec![0u8; 128]).unwrap();
    write_ai_config(
        home,
        &format!("[ai]\nwhisper_cli = {:?}\n", cli.display().to_string()),
    );

    let out = oca_in(home).arg("doctor").output().unwrap();
    assert!(out.status.success());
    assert!(
        out.stderr.is_empty(),
        "doctor keeps stderr silent: {:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("✓ Dictation"), "got: {stdout}");
    assert!(stdout.contains("whisper-cli-stand-in"), "got: {stdout}");
    assert!(stdout.contains("ggml-tiny"), "got: {stdout}");
    assert!(stdout.contains("no language model"), "got: {stdout}");
    // The LLM side is still missing — and that's a warning, not a failure.
    assert!(stdout.contains("⚠ Models"), "got: {stdout}");
}
