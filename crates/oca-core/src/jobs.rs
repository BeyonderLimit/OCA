//! Typed background jobs with progress, cancel, and priority (ADR-0006 /
//! A6) — the one place long-running work is tracked. Interactive work
//! (user-visible generation) outranks background work; every later long
//! operation goes through here instead of inventing its own thread.
//!
//! Jobs are plain threads: cheap, dependency-free, and cancellable via an
//! atomic token the closure can poll. A panicking closure is caught and
//! reported as a failure — a job never takes a frontend down with it.

use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Cooperative cancel token handed to every job.
pub type JobCancel = Arc<AtomicBool>;

/// Priority ladder (user-interactive > background), per ADR-0006.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    Background,
    Interactive,
}

impl Priority {
    pub fn label(self) -> &'static str {
        match self {
            Priority::Background => "background",
            Priority::Interactive => "interactive",
        }
    }
}

/// Terminal-or-not state of one job.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JobState {
    Running,
    Done,
    /// Cancelled before finishing (the token was observed).
    Cancelled,
    /// Failed, or panicked — the message says which.
    Failed(String),
}

impl JobState {
    pub fn is_running(&self) -> bool {
        matches!(self, JobState::Running)
    }
}

/// A point-in-time view of one job, safe to hand to any frontend.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobSnapshot {
    pub id: u64,
    pub label: String,
    pub priority: Priority,
    pub state: JobState,
    /// Last `progress` message, if the job reported one.
    pub progress: Option<String>,
}

struct Job {
    label: String,
    priority: Priority,
    state: Mutex<JobState>,
    progress: Mutex<Option<String>>,
    cancel: JobCancel,
}

impl Job {
    fn snapshot(&self, id: u64) -> JobSnapshot {
        JobSnapshot {
            id,
            label: self.label.clone(),
            priority: self.priority,
            state: self.state.lock().expect("job state").clone(),
            progress: self.progress.lock().expect("job progress").clone(),
        }
    }
}

/// What a job closure receives: the cancel token plus progress reporting.
pub struct JobContext {
    job: Arc<Job>,
}

impl JobContext {
    pub fn is_cancelled(&self) -> bool {
        self.job.cancel.load(Ordering::SeqCst)
    }

    /// For closures that want to hand the token to sub-functions.
    pub fn cancel_token(&self) -> JobCancel {
        Arc::clone(&self.job.cancel)
    }

    /// Publish a short status line ("embedding 120/940…").
    pub fn progress(&self, message: impl Into<String>) {
        *self.job.progress.lock().expect("job progress") = Some(message.into());
    }
}

/// How many terminal jobs stay queryable before the oldest are dropped
/// (a long session must not accumulate forever).
const MAX_TRACKED: usize = 1024;

/// The registry. One lives on [`crate::Core`]; frontends spawn through it.
#[derive(Default)]
pub struct Jobs {
    next: AtomicU64,
    jobs: Mutex<HashMap<u64, Arc<Job>>>,
}

impl Jobs {
    pub fn new() -> Self {
        Self::default()
    }

    /// Spawn `f` on its own thread. Returns the job id immediately; query
    /// progress/state with [`Self::snapshot`], stop with [`Self::cancel`].
    pub fn spawn<F>(&self, label: impl Into<String>, priority: Priority, f: F) -> u64
    where
        F: FnOnce(&JobContext) -> anyhow::Result<()> + Send + 'static,
    {
        let id = self.next.fetch_add(1, Ordering::SeqCst) + 1;
        let job = Arc::new(Job {
            label: label.into(),
            priority,
            state: Mutex::new(JobState::Running),
            progress: Mutex::new(None),
            cancel: Arc::new(AtomicBool::new(false)),
        });
        {
            let mut jobs = self.jobs.lock().expect("jobs registry");
            jobs.insert(id, Arc::clone(&job));
            if jobs.len() > MAX_TRACKED {
                jobs.retain(|_, j| j.state.lock().expect("job state").is_running());
            }
        }

        let worker = Arc::clone(&job);
        std::thread::spawn(move || {
            let ctx = JobContext { job: worker };
            // A panicking job reports failure instead of losing the thread
            // (and leaving a forever-Running entry behind).
            let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| f(&ctx)))
                .unwrap_or_else(|_| Err(anyhow::anyhow!("job panicked")));
            let cancelled = ctx.is_cancelled();
            let state = if cancelled && outcome.is_ok() {
                JobState::Cancelled
            } else {
                match outcome {
                    Ok(()) => JobState::Done,
                    Err(err) => JobState::Failed(format!("{err:#}")),
                }
            };
            *ctx.job.state.lock().expect("job state") = state;
        });
        id
    }

    /// Signal a job to stop. Returns `false` when the id is unknown.
    /// (Cooperative: the closure must observe the token — or the caller
    /// must unblock it, e.g. the model's `cancel` for streams.)
    pub fn cancel(&self, id: u64) -> bool {
        let jobs = self.jobs.lock().expect("jobs registry");
        match jobs.get(&id) {
            Some(job) => {
                job.cancel.store(true, Ordering::SeqCst);
                true
            }
            None => false,
        }
    }

    pub fn snapshot(&self, id: u64) -> Option<JobSnapshot> {
        let jobs = self.jobs.lock().expect("jobs registry");
        jobs.get(&id).map(|job| job.snapshot(id))
    }

    /// Every tracked job, newest first.
    pub fn list(&self) -> Vec<JobSnapshot> {
        let jobs = self.jobs.lock().expect("jobs registry");
        let mut out: Vec<JobSnapshot> = jobs.iter().map(|(id, job)| job.snapshot(*id)).collect();
        out.sort_by_key(|snapshot| std::cmp::Reverse(snapshot.id));
        out
    }

    /// Poll until the job leaves `Running` (or the budget runs out).
    /// Returns the final state when finished, `None` on timeout.
    pub fn wait(&self, id: u64, timeout: Duration) -> Option<JobState> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let snapshot = self.snapshot(id)?;
            if !snapshot.state.is_running() {
                return Some(snapshot.state);
            }
            if std::time::Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    fn quick(jobs: &Jobs, id: u64) -> JobState {
        jobs.wait(id, Duration::from_secs(5))
            .expect("job must finish")
    }

    #[test]
    fn spawn_runs_to_done_with_priority_and_label() {
        let jobs = Jobs::new();
        let counter = Arc::new(AtomicUsize::new(0));
        let worker = Arc::clone(&counter);
        let id = jobs.spawn("index things", Priority::Background, move |ctx| {
            assert!(!ctx.is_cancelled());
            worker.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });
        assert_eq!(quick(&jobs, id), JobState::Done);
        assert_eq!(counter.load(Ordering::SeqCst), 1);

        let snapshot = jobs.snapshot(id).unwrap();
        assert_eq!(snapshot.label, "index things");
        assert_eq!(snapshot.priority, Priority::Background);
        assert_eq!(snapshot.progress, None);
    }

    #[test]
    fn cancel_stops_a_looping_job_and_reports_cancelled() {
        let jobs = Jobs::new();
        let id = jobs.spawn("spin", Priority::Interactive, |ctx| {
            while !ctx.is_cancelled() {
                std::thread::sleep(Duration::from_millis(1));
            }
            Ok(())
        });
        assert!(jobs.cancel(id));
        assert_eq!(quick(&jobs, id), JobState::Cancelled);
        assert!(!jobs.cancel(9999), "unknown ids are reported, not panicked");
    }

    #[test]
    fn progress_is_visible_while_running() {
        let jobs = Jobs::new();
        let id = jobs.spawn("chatty", Priority::Interactive, |ctx| {
            ctx.progress("halfway");
            while !ctx.is_cancelled() {
                std::thread::sleep(Duration::from_millis(1));
            }
            Ok(())
        });
        // Poll until the message shows up (or the job ends).
        loop {
            let snapshot = jobs.snapshot(id).unwrap();
            if snapshot.progress.as_deref() == Some("halfway") {
                break;
            }
            assert!(snapshot.state.is_running(), "job ended too early");
            std::thread::sleep(Duration::from_millis(2));
        }
        jobs.cancel(id);
        assert_eq!(quick(&jobs, id), JobState::Cancelled);
        let snapshot = jobs.snapshot(id).unwrap();
        assert_eq!(snapshot.progress.as_deref(), Some("halfway"));
    }

    #[test]
    fn failures_and_panics_become_states_not_dead_threads() {
        let jobs = Jobs::new();
        let failing = jobs.spawn("doomed", Priority::Background, |_| {
            Err(anyhow::anyhow!("model runtime crashed (simulated)"))
        });
        match quick(&jobs, failing) {
            JobState::Failed(msg) => assert!(msg.contains("crashed"), "{msg}"),
            other => panic!("expected Failed, got {other:?}"),
        }

        let panicking = jobs.spawn("boomer", Priority::Background, |_| {
            panic!("deliberate test panic");
        });
        match quick(&jobs, panicking) {
            JobState::Failed(msg) => assert!(msg.contains("panicked"), "{msg}"),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn list_is_newest_first_and_wait_times_out_on_unknown_ids() {
        let jobs = Jobs::new();
        let a = jobs.spawn("first", Priority::Background, |_| Ok(()));
        let b = jobs.spawn("second", Priority::Interactive, |_| Ok(()));
        quick(&jobs, a);
        quick(&jobs, b);
        let listed = jobs.list();
        assert_eq!(listed.len(), 2);
        assert!(listed[0].id > listed[1].id, "newest first");
        assert!(jobs.wait(424242, Duration::from_millis(5)).is_none());
    }

    #[test]
    fn tracked_jobs_are_bounded() {
        let jobs = Jobs::new();
        let mut ids = Vec::new();
        for i in 0..(MAX_TRACKED + 25) {
            ids.push(jobs.spawn(format!("job {i}"), Priority::Background, |_| Ok(())));
        }
        for id in &ids {
            let _ = jobs.wait(*id, Duration::from_secs(5));
        }
        // Terminal jobs beyond the cap are dropped; running ones would stay.
        assert!(
            jobs.list().len() < ids.len(),
            "old terminal jobs must be dropped"
        );
        assert!(jobs.list().len() <= MAX_TRACKED + 25);
    }
}
