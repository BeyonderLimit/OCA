//! `oca doctor` — per-subsystem health report (Phase 0 stubs).
//!
//! Subsystems whose phases haven't landed yet report `⚠` with the phase that
//! delivers them; real problems (bad config, unwritable log dir) surface as
//! `⚠` details or fail earlier in config loading. Exit code is 0: warnings are
//! informational until later phases make them real checks.

use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::logging::Logging;

pub struct Check {
    pub ok: bool,
    pub label: &'static str,
    pub detail: String,
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

    checks.push(match project_dir {
        // Opening the cache DB is safe for `doctor`: it only creates or
        // repairs `.oca/cache/oca.db` (deletable, rebuildable — rule 2).
        Some(dir) => match oca_storage::db::Db::open(dir) {
            Ok(db) => match db.totals() {
                Ok(t) if t.documents == 0 && t.units == 0 => Check {
                    ok: false,
                    label: "Database",
                    detail: "no index yet — run `oca reindex`".into(),
                },
                Ok(t) => Check {
                    ok: true,
                    label: "Database",
                    detail: format!(
                        ".oca/cache/oca.db — {} documents, {} units, {} tags, {} takes",
                        t.documents, t.units, t.tags, t.takes
                    ),
                },
                Err(err) => Check {
                    ok: false,
                    label: "Database",
                    detail: format!("cache index unreadable ({err:#})"),
                },
            },
            Err(err) => Check {
                ok: false,
                label: "Database",
                detail: format!("cache index unavailable ({err:#})"),
            },
        },
        None => Check {
            ok: false,
            label: "Database",
            detail: "no project here".into(),
        },
    });

    // Snapshots (Phase 6): the store is created lazily on first save.
    checks.push(match project_dir {
        Some(dir) => match oca_core::snapshot_summary(dir) {
            Ok((0, _)) => Check {
                ok: false,
                label: "Snapshots",
                detail: "none yet — save one with `oca snapshot create`".into(),
            },
            Ok((count, _)) => Check {
                ok: true,
                label: "Snapshots",
                detail: format!("{count} saved (restore points)"),
            },
            Err(err) => Check {
                ok: false,
                label: "Snapshots",
                detail: format!("snapshot store unreadable ({err})"),
            },
        },
        None => Check {
            ok: false,
            label: "Snapshots",
            detail: "no project here".into(),
        },
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

    // Model runtime (Phase 7): discovery only — doctor never spawns it.
    let runtime_dir = cfg
        .ai
        .runtime_dir
        .clone()
        .or_else(|| home.map(|h| h.join(".oca").join("runtime")))
        .unwrap_or_else(oca_ai::default_runtime_dir);
    let models_dir = cfg
        .ai
        .models_dir
        .clone()
        .or_else(|| home.map(|h| h.join(".oca").join("models").join("llm")))
        .unwrap_or_else(oca_ai::default_models_dir);
    let server: Option<PathBuf> = match &cfg.ai.llama_server {
        // An explicit config path must exist *as configured* — report the
        // miss rather than silently using a different runtime.
        Some(path) if path.is_file() => Some(path.clone()),
        Some(_) => None,
        None => oca_ai::find_server_in(&runtime_dir, std::env::var_os("PATH").as_deref()),
    };
    let configured_missing = cfg.ai.llama_server.as_ref().is_some_and(|p| !p.is_file());
    checks.push(match server {
        Some(path) => Check {
            ok: true,
            label: "Model runtime",
            detail: format!("{} · one heavy model at a time", path.display()),
        },
        None if configured_missing => Check {
            ok: false,
            label: "Model runtime",
            detail: format!(
                "configured path missing: {}",
                cfg.ai
                    .llama_server
                    .as_deref()
                    .unwrap_or(Path::new(""))
                    .display()
            ),
        },
        None => Check {
            ok: false,
            label: "Model runtime",
            detail: "not installed — side-load with `oca runtime install --from <pack.tar.gz>`"
                .into(),
        },
    });

    // Installed LLM models (~/.oca/models/llm, largest first).
    checks.push(match oca_ai::discover_models(&models_dir) {
        Ok(models) if models.is_empty() => Check {
            ok: false,
            label: "Models",
            detail: format!(
                "no model in {} — drop a .gguf file in",
                models_dir.display()
            ),
        },
        Ok(models) => {
            let mut shown: Vec<String> = models
                .iter()
                .take(3)
                .map(|m| format!("{} ({})", m.name, oca_ai::human_size(m.bytes)))
                .collect();
            if models.len() > 3 {
                shown.push(format!("+{} more", models.len() - 3));
            }
            Check {
                ok: true,
                label: "Models",
                detail: format!("{} — {}", models_dir.display(), shown.join(", ")),
            }
        }
        Err(err) => Check {
            ok: false,
            label: "Models",
            detail: format!("models directory unreadable ({err:#})"),
        },
    });

    // Hardware tier recommendation (ADR-0008): tiers, not model names.
    {
        let hw = oca_ai::Hardware::detect();
        let detail = match hw.tier() {
            Some(tier) => format!(
                "{} GB RAM × {} cores → {}",
                hw.ram_gb(),
                hw.cores,
                tier.describe()
            ),
            None => format!("RAM unknown × {} cores", hw.cores),
        };
        checks.push(Check {
            ok: true,
            label: "Hardware",
            detail,
        });
    }

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

    fn cfg_with_runtime(path: &std::path::Path) -> Config {
        let mut cfg = Config::default();
        cfg.ai.llama_server = Some(path.to_path_buf());
        cfg
    }

    #[test]
    fn ai_checks_report_missing_runtime_models_and_hardware() {
        let tmp = TempDir::new().unwrap();
        let missing = tmp.path().join("no-llama-server");
        let cfg = cfg_with_runtime(&missing);
        let checks = collect(&cfg, &logging_ok(), None, Some(tmp.path()));

        let runtime = checks.iter().find(|c| c.label == "Model runtime").unwrap();
        assert!(!runtime.ok);
        assert!(
            runtime.detail.contains("configured path missing"),
            "{}",
            runtime.detail
        );

        let models = checks.iter().find(|c| c.label == "Models").unwrap();
        assert!(!models.ok);
        assert!(models.detail.contains("drop a .gguf"), "{}", models.detail);

        let hw = checks.iter().find(|c| c.label == "Hardware").unwrap();
        assert!(hw.ok);
        assert!(hw.detail.contains("cores"), "{}", hw.detail);
    }

    #[test]
    fn ai_checks_pass_with_runtime_and_model() {
        let tmp = TempDir::new().unwrap();
        // A stand-in runtime binary and one discovered model.
        let server = tmp.path().join("llama-server");
        std::fs::write(&server, b"#!/bin/sh\n").unwrap();
        let models = tmp.path().join(".oca").join("models").join("llm");
        std::fs::create_dir_all(&models).unwrap();
        std::fs::write(models.join("lyra-3b.gguf"), vec![0u8; 2048]).unwrap();

        let cfg = cfg_with_runtime(&server);
        let checks = collect(&cfg, &logging_ok(), None, Some(tmp.path()));

        let runtime = checks.iter().find(|c| c.label == "Model runtime").unwrap();
        assert!(runtime.ok, "{}", runtime.detail);
        assert!(
            runtime.detail.contains("one heavy model at a time"),
            "{}",
            runtime.detail
        );

        let models = checks.iter().find(|c| c.label == "Models").unwrap();
        assert!(models.ok, "{}", models.detail);
        assert!(models.detail.contains("lyra-3b"), "{}", models.detail);
        assert!(models.detail.contains("2.0 KB"), "{}", models.detail);
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
