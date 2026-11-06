//! Behaviour tests for the async job bridge.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, LazyLock, Mutex, PoisonError};
use std::time::{Duration, Instant};

use super::*;

#[derive(Default)]
struct RecordingSink {
    events: Mutex<Vec<(Instant, JobEvent)>>,
    changed: Condvar,
}

impl RecordingSink {
    fn events(&self) -> Vec<(Instant, JobEvent)> {
        lock(&self.events).clone()
    }

    fn finished_count(&self) -> usize {
        self.events()
            .iter()
            .filter(|(_, event)| matches!(event, JobEvent::Finished(_)))
            .count()
    }

    fn wait_for_finished(&self, timeout: Duration) -> JobFinishedEvent {
        self.wait_for_finished_id(None, timeout)
    }

    fn wait_for_finished_id(&self, id: Option<&JobId>, timeout: Duration) -> JobFinishedEvent {
        let deadline = Instant::now() + timeout;
        let mut events = lock(&self.events);
        loop {
            if let Some(finished) = events.iter().find_map(|(_, event)| match event {
                JobEvent::Finished(finished) if id.is_none_or(|id| finished.id == *id) => {
                    Some(finished.clone())
                }
                _ => None,
            }) {
                return finished;
            }
            let now = Instant::now();
            let Some(remaining) = deadline.checked_duration_since(now) else {
                panic!(
                    "timed out waiting for a finished event; saw {:?}",
                    events
                        .iter()
                        .map(|(_, event)| event.name())
                        .collect::<Vec<_>>()
                );
            };
            let (guard, _) = self
                .changed
                .wait_timeout(events, remaining)
                .unwrap_or_else(PoisonError::into_inner);
            events = guard;
        }
    }
}

impl EventSink for RecordingSink {
    fn emit(&self, event: JobEvent) {
        lock(&self.events).push((Instant::now(), event));
        self.changed.notify_all();
    }
}

#[derive(Default)]
struct RecordingOperations {
    rows: Mutex<Vec<InterruptedJob>>,
}

impl OperationsLog for RecordingOperations {
    fn record_interrupted(&self, job: &InterruptedJob) -> Result<(), OperationsLogError> {
        lock(&self.rows).push(job.clone());
        Ok(())
    }
}

struct DropFlag(Arc<AtomicBool>);

impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[test]
fn completion_emits_started_once_and_finished_once() {
    let recorder = Arc::new(RecordingSink::default());
    let bridge = JobBridge::new(recorder.clone(), Arc::new(RecordingOperations::default()));

    let handle = bridge
        .spawn_job(JobSpec::new("Index skills", |progress| async move {
            progress.fraction(0.5, "halfway");
            Ok(())
        }))
        .expect("spawn");

    let finished = recorder.wait_for_finished(Duration::from_secs(2));
    assert_eq!(finished.id, *handle.id());
    assert_eq!(finished.state, JobState::Succeeded);
    assert_eq!(finished.error, None);

    let events = recorder.events();
    assert_eq!(
        events
            .iter()
            .filter(|(_, event)| matches!(event, JobEvent::Started(_)))
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|(_, event)| matches!(event, JobEvent::Finished(_)))
            .count(),
        1
    );
    assert!(
        events
            .iter()
            .filter(|(_, event)| matches!(event, JobEvent::Progress(_)))
            .count()
            <= 10
    );
    bridge.shutdown(Duration::ZERO);
}

#[test]
fn ten_thousand_progress_ticks_are_coalesced_to_at_most_ten_per_second() {
    let recorder = Arc::new(RecordingSink::default());
    let bridge = JobBridge::new(recorder.clone(), Arc::new(RecordingOperations::default()));

    bridge
        .spawn_job(JobSpec::new("Index 10k", |progress| async move {
            let start = Instant::now();
            let mut ticks = 0_usize;
            while ticks < 10_000 || start.elapsed() < Duration::from_millis(1_200) {
                for _ in 0..84 {
                    progress.message("indexing");
                    ticks += 1;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Ok(())
        }))
        .expect("spawn");

    let finished = recorder.wait_for_finished(Duration::from_secs(5));
    assert_eq!(finished.state, JobState::Succeeded);

    let events = recorder.events();
    assert_eq!(recorder.finished_count(), 1);
    let progress: Vec<Instant> = events
        .iter()
        .filter_map(|(at, event)| matches!(event, JobEvent::Progress(_)).then_some(*at))
        .collect();
    assert!(
        progress.len() >= 5,
        "expected periodic coalesced progress, got {}",
        progress.len()
    );
    for window_start in &progress {
        let in_window = progress
            .iter()
            .filter(|at| **at >= *window_start && **at - *window_start < Duration::from_secs(1))
            .count();
        assert!(
            in_window <= 10,
            "{in_window} progress events inside one second"
        );
    }
    bridge.shutdown(Duration::ZERO);
}

#[test]
fn cancel_stops_the_job_and_drops_the_late_result() {
    let recorder = Arc::new(RecordingSink::default());
    let bridge = JobBridge::new(recorder.clone(), Arc::new(RecordingOperations::default()));
    let dropped = Arc::new(AtomicBool::new(false));
    let flag = DropFlag(Arc::clone(&dropped));

    let handle = bridge
        .spawn_job(JobSpec::new("Wait forever", move |_progress| {
            let flag = flag;
            async move {
                let _flag = flag;
                tokio::time::sleep(Duration::from_secs(30)).await;
                Ok(())
            }
        }))
        .expect("spawn");

    std::thread::sleep(Duration::from_millis(20));
    let started = Instant::now();
    handle.cancel();
    let finished = recorder.wait_for_finished(Duration::from_millis(100));
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_millis(1000),
        "cancel took {elapsed:?}"
    );
    assert_eq!(finished.state, JobState::Cancelled);
    assert!(handle.is_cancelled());

    let deadline = Instant::now() + Duration::from_millis(200);
    while !dropped.load(Ordering::SeqCst) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(
        dropped.load(Ordering::SeqCst),
        "the cancelled job future was not dropped"
    );

    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(recorder.finished_count(), 1, "late result was not dropped");
    bridge.shutdown(Duration::ZERO);
}

static LOG_BUFFER: LazyLock<Arc<Mutex<Vec<u8>>>> = LazyLock::new(|| {
    let buffer = Arc::new(Mutex::new(Vec::new()));
    let writer = Arc::clone(&buffer);
    let _ = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_max_level(tracing::Level::TRACE)
        .with_writer(move || SharedWriter(Arc::clone(&writer)))
        .try_init();
    buffer
});

fn log_buffer() -> Arc<Mutex<Vec<u8>>> {
    Arc::clone(&LOG_BUFFER)
}

struct SharedWriter(Arc<Mutex<Vec<u8>>>);

impl Write for SharedWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        lock(&self.0).extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn panic_becomes_failed_panic_and_the_bridge_survives() {
    let logs = log_buffer();
    let recorder = Arc::new(RecordingSink::default());
    let bridge = JobBridge::new(recorder.clone(), Arc::new(RecordingOperations::default()));

    let handle = bridge
        .spawn_job(JobSpec::new("Explode", |_progress| async move {
            panic!("boom from job")
        }))
        .expect("spawn");

    let finished = recorder.wait_for_finished(Duration::from_secs(2));
    assert_eq!(finished.id, *handle.id());
    assert_eq!(finished.state, JobState::Failed);
    assert_eq!(
        finished.error,
        Some(JobError::Panic {
            message: "boom from job".to_owned()
        })
    );

    let text = String::from_utf8_lossy(&lock(&logs)).into_owned();
    assert!(
        text.contains(handle.id().as_str()),
        "log does not mention the job id: {text}"
    );
    assert!(
        text.contains("boom from job"),
        "log does not mention the panic text: {text}"
    );

    let second = bridge
        .spawn_job(JobSpec::new("After panic", |_progress| async { Ok(()) }))
        .expect("spawn after panic");
    let finished = recorder.wait_for_finished_id(Some(second.id()), Duration::from_secs(2));
    assert_eq!(finished.state, JobState::Succeeded);
    bridge.shutdown(Duration::ZERO);
}

#[test]
fn quitting_interrupts_every_in_flight_job_exactly_once() {
    let recorder = Arc::new(RecordingSink::default());
    let operations = Arc::new(RecordingOperations::default());
    let bridge = JobBridge::new(recorder.clone(), operations.clone());

    let mut ids = Vec::new();
    for index in 0..3 {
        let handle = bridge
            .spawn_job(JobSpec::new(
                format!("Long job {index}"),
                |_progress| async {
                    tokio::time::sleep(Duration::from_secs(60)).await;
                    Ok(())
                },
            ))
            .expect("spawn");
        ids.push(handle.id().clone());
    }

    let started = Instant::now();
    let report = bridge.shutdown(Duration::from_secs(5));
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "shutdown exceeded the grace budget: {:?}",
        started.elapsed()
    );
    assert_eq!(report.interrupted, 3);

    let rows = lock(&operations.rows).clone();
    assert_eq!(rows.len(), 3);
    for id in &ids {
        assert_eq!(
            rows.iter().filter(|row| row.id == *id).count(),
            1,
            "expected exactly one operations row for {id}"
        );
    }

    let events = recorder.events();
    for id in &ids {
        let states: Vec<JobState> = events
            .iter()
            .filter_map(|(_, event)| match event {
                JobEvent::Finished(finished) if finished.id == *id => Some(finished.state),
                _ => None,
            })
            .collect();
        assert_eq!(states, vec![JobState::Interrupted]);
    }

    assert!(
        bridge
            .spawn_job(JobSpec::new("After shutdown", |_progress| async { Ok(()) }))
            .is_err()
    );
    assert_eq!(bridge.shutdown(Duration::from_secs(1)).interrupted, 0);
}

#[test]
fn returned_error_becomes_failed_with_the_error_payload() {
    let recorder = Arc::new(RecordingSink::default());
    let bridge = JobBridge::new(recorder.clone(), Arc::new(RecordingOperations::default()));

    let handle = bridge
        .spawn_job(JobSpec::new("Validate store", |_progress| async move {
            Err(JobError::Failed {
                message: "store.json is unreadable".to_owned(),
            })
        }))
        .expect("spawn");

    let finished = recorder.wait_for_finished(Duration::from_secs(2));
    assert_eq!(finished.id, *handle.id());
    assert_eq!(finished.state, JobState::Failed);
    assert_eq!(
        finished.error,
        Some(JobError::Failed {
            message: "store.json is unreadable".to_owned()
        })
    );
    bridge.shutdown(Duration::ZERO);
}

#[test]
fn jobs_lists_running_work_and_cancel_by_id_is_idempotent() {
    let recorder = Arc::new(RecordingSink::default());
    let bridge = JobBridge::new(recorder.clone(), Arc::new(RecordingOperations::default()));

    let handle = bridge
        .spawn_job(JobSpec::new("Long job", |_progress| async {
            tokio::time::sleep(Duration::from_secs(30)).await;
            Ok(())
        }))
        .expect("spawn");

    let jobs = bridge.jobs();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].id, *handle.id());
    assert_eq!(jobs[0].name, "Long job");
    assert_eq!(jobs[0].state, JobState::Running);

    assert!(bridge.cancel_job(handle.id()));
    assert!(!bridge.cancel_job(handle.id()));
    assert!(bridge.jobs().is_empty());
    assert_eq!(recorder.finished_count(), 1);
    bridge.shutdown(Duration::ZERO);
}
