//! `/_console/api/jobs` (feature `jobs`): per-queue counts and the newest jobs of an
//! `id_effect_jobs` [`JobQueue`], mapped to console view types.

use std::collections::BTreeMap;
use std::sync::Arc;

use id_effect::{Exit, UtcDateTime};
use id_effect_axum::EffectExecutor;
use id_effect_jobs::{JobFilter, JobQueue, JobRecord, QueueStats};
use serde::{Deserialize, Serialize};

/// How many of the newest jobs `GET jobs` lists.
pub(crate) const RECENT_JOBS: u32 = 50;

/// What `GET /_console/api/jobs` answers.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobsView {
  /// One entry per queue, by queue name.
  pub queues: Vec<QueueView>,
  /// The newest jobs of every queue, newest first (at most 50).
  pub recent: Vec<JobView>,
}

/// One queue's counts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueView {
  /// The queue name.
  pub queue: String,
  /// Jobs per state (`scheduled`, `running`, `succeeded`, `failed`, `dead`, `cancelled`); states
  /// without jobs are absent.
  pub counts: BTreeMap<String, u64>,
  /// The earliest `run_at` among the runnable jobs (RFC 3339).
  pub oldest_runnable: Option<String>,
}

impl From<QueueStats> for QueueView {
  fn from(stats: QueueStats) -> Self {
    Self {
      queue: stats.queue.to_string(),
      counts: stats
        .counts
        .into_iter()
        .map(|(state, n)| (state.as_str().to_owned(), n))
        .collect(),
      oldest_runnable: stats.oldest_runnable.map(rfc3339),
    }
  }
}

/// One job, without its payload (which may carry personal data).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobView {
  /// The id.
  pub id: i64,
  /// The job's name.
  pub name: String,
  /// The queue.
  pub queue: String,
  /// The state.
  pub state: String,
  /// The priority (higher runs first).
  pub priority: i16,
  /// Attempts started so far.
  pub attempts: u16,
  /// The maximum number of attempts.
  pub max_attempts: u16,
  /// When it may run next (RFC 3339).
  pub run_at: String,
  /// When it was enqueued (RFC 3339).
  pub inserted_at: String,
  /// When it finished (RFC 3339).
  pub finished_at: Option<String>,
  /// The worker running it.
  pub locked_by: Option<String>,
  /// The text of the last failure.
  pub last_error: Option<String>,
}

impl From<JobRecord> for JobView {
  fn from(job: JobRecord) -> Self {
    Self {
      id: job.id.0,
      name: job.name,
      queue: job.queue.to_string(),
      state: job.state.as_str().to_owned(),
      priority: job.priority.0,
      attempts: job.attempts,
      max_attempts: job.max_attempts,
      run_at: rfc3339(job.run_at),
      inserted_at: rfc3339(job.inserted_at),
      finished_at: job.finished_at.map(rfc3339),
      locked_by: job.locked_by,
      last_error: job.last_error,
    }
  }
}

fn rfc3339(at: UtcDateTime) -> String {
  at.0.to_string()
}

/// The queue's stats and newest jobs, or the text of the queue's failure.
pub(crate) async fn snapshot(queue: &Arc<dyn JobQueue>) -> Result<JobsView, String> {
  let executor = EffectExecutor::global();
  let stats = match executor.run_effect(queue.stats()).await {
    Exit::Success(stats) => stats,
    Exit::Failure(cause) => return Err(format!("job queue stats failed: {cause:?}")),
  };
  let filter = JobFilter {
    limit: RECENT_JOBS,
    ..JobFilter::default()
  };
  let recent = match executor.run_effect(queue.list(filter)).await {
    Exit::Success(jobs) => jobs,
    Exit::Failure(cause) => return Err(format!("job queue list failed: {cause:?}")),
  };
  Ok(JobsView {
    queues: stats.into_iter().map(QueueView::from).collect(),
    recent: recent.into_iter().map(JobView::from).collect(),
  })
}
