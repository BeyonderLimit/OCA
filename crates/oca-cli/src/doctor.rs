//! `oca doctor` — per-subsystem health report (Phase 0 stubs).
//!
//! Subsystems whose phases haven't landed yet report `⚠` with the phase that
//! delivers them; real problems (bad config, unwritable log dir) surface as
//! `⚠` details or fail earlier in config loading. Exit code is 0: warnings are
//! informational until later phases make them real checks.

use std::path::Path;

use crate::config::Config;
use crate::logging::Logging;

pub struct Check {
    pub ok: bool,
    pub label: &'static str,
    pub detail: String,
}

/// Path a future feature will live at, for stub details.
fn phase(what: &str, phase: &str) -> String {
    format!("{what} arrives in {phase}")
}

pub fn collect(
    cfg: &Config,
    logging: &Logging,
    project_dir: Option<&Path>,
    home: Option<&Path>,
) -> Vec<Check> {
    let mut checks = Vec::new();

    checks.push(Check {
        ok: true,
        label: "Config",
        detail: cfg.sources_detail(),
    });

    checks.push(match &logging.file {
        Ok(path) => Check {
            ok: true,
            label: "Logging",
            detail: format!("{} (daily-rolled)", path.display()),
        },
        Err(err) => Check {
            ok: false,
            label: "Logging",
            detail: err.clone(),
        },
    });

    checks.push(match project_dir {
        Some(dir) => Check {
            ok: true,
            label: "Project directory",
            detail: format!("{}", dir.display()),
        },
        None => Check {
            ok: false,
            label: "Project directory",
            detail: "no project here (`oca new` arrives in Phase 1)".into(),
        },
    });

    checks.push(Check {
        ok: false,
        label: "Database",
        detail: phase("SQLite index (FTS5)", "Phase 5"),
    });

    checks.push(Check {
        ok: false,
        label: "Git",
        detail: phase("snapshot versioning", "Phase 6"),
    });

    checks.push({
        let report = oca_audio::Audio::probe(&oca_audio::CpalHost);
        let input = match &report.input {
            Ok(spec) => format!("input ✓ {} Hz × {} ch", spec.sample_rate, spec.channels),
            Err(err) => format!("input unavailable ({err})"),
        };
        let output = match &report.output {
            Ok(spec) => format!("output ✓ {} Hz × {} ch", spec.sample_rate, spec.channels),
            Err(err) => format!("output unavailable ({err})"),
        };
        Check {
            ok: report.available(),
            label: "Audio subsystem",
            detail: format!("{input} · {output}"),
        }
    });

    checks.push(Check {
        ok: false,
        label: "Model runtime",
        detail: phase("optional AI runtime (llama-server)", "Phase 7"),
    });

    // Embeddings: a real check today — presence of `~/.oca/models/embeddings/`.
    let embeddings = home.map(|h| h.join(".oca").join("models").join("embeddings"));
    checks.push(match embeddings {
        Some(dir) if dir.is_dir() && dir.read_dir().is_ok_and(|mut d| d.next().is_some()) => {
            Check {
                ok: true,
                label: "Embeddings",
                detail: dir.display().to_string(),
            }
        }
        Some(_) | None => Check {
            ok: false,
            label: "Embeddings",
            detail: "No embedding model installed".into(),
        },
    });

    checks
}

pub fn render(checks: &[Check]) -> String {
    let mut out = String::from("OCA Doctor\n");
    for c in checks {
        let mark = if c.ok { "✓" } else { "⚠" };
        if c.detail.is_empty() {
            out.push_str(&format!("{mark} {}\n", c.label));
        } else {
            out.push_str(&format!("{mark} {} — {}\n", c.label, c.detail));
        }
    }
    out
}

pub fn run(cfg: &Config, logging: &Logging, project_dir: Option<&Path>, home: Option<&Path>) -> u8 {
    let checks = collect(cfg, logging, project_dir, home);
    print!("{}", render(&checks));
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::logging::Logging;
    use tempfile::TempDir;

    fn logging_ok() -> Logging {
        Logging {
            stderr_level: "warn".into(),
            file: Ok(Path::new("/tmp/oca.log").to_path_buf()),
            _guard: None,
        }
    }

    #[test]
    fn render_matches_mock_shape() {
        let checks = vec![
            Check {
                ok: true,
                label: "Database",
                detail: String::new(),
            },
            Check {
                ok: false,
                label: "Embeddings",
                detail: "No embedding model installed".into(),
            },
        ];
        let out = render(&checks);
        assert!(out.starts_with("OCA Doctor\n"));
        assert!(out.contains("✓ Database\n"));
        assert!(out.contains("⚠ Embeddings — No embedding model installed\n"));
    }

    #[test]
    fn no_project_and_no_models_are_warnings() {
        let tmp = TempDir::new().unwrap();
        let checks = collect(&Config::default(), &logging_ok(), None, Some(tmp.path()));
        let project = checks
            .iter()
            .find(|c| c.label == "Project directory")
            .unwrap();
        assert!(!project.ok);
        let emb = checks.iter().find(|c| c.label == "Embeddings").unwrap();
        assert!(!emb.ok);
        assert_eq!(emb.detail, "No embedding model installed");
    }

    #[test]
    fn embeddings_dir_with_file_is_ok() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join(".oca").join("models").join("embeddings");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("model.bin"), []).unwrap();
        let checks = collect(&Config::default(), &logging_ok(), None, Some(tmp.path()));
        let emb = checks.iter().find(|c| c.label == "Embeddings").unwrap();
        assert!(emb.ok);
    }

    #[test]
    fn detected_project_is_reported() {
        let tmp = TempDir::new().unwrap();
        let checks = collect(
            &Config::default(),
            &logging_ok(),
            Some(tmp.path()),
            Some(tmp.path()),
        );
        let project = checks
            .iter()
            .find(|c| c.label == "Project directory")
            .unwrap();
        assert!(project.ok);
        assert!(project.detail.contains(&tmp.path().display().to_string()));
    }
}
