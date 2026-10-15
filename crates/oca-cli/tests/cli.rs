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
