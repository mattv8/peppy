use peppy_desktop_api::{LaunchPhase, LaunchProgress, LaunchStep, LaunchUpdate};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

pub(crate) const EVENT: &str = "peppy://launch-progress";
const UPDATE_INTERVAL: Duration = Duration::from_millis(250);
type Emitter = Arc<dyn Fn(LaunchProgress) + Send + Sync>;

pub(crate) struct LaunchTracker {
    started: Instant,
    state: Mutex<TrackerState>,
}

struct TrackerState {
    phase: LaunchPhase,
    phase_started: Instant,
    steps: Vec<LaunchStep>,
    update: Option<LaunchUpdate>,
    updated_rows: Option<u64>,
    last_update_emitted: Option<Instant>,
    emitter: Option<Emitter>,
}

impl Default for LaunchTracker {
    fn default() -> Self {
        let now = Instant::now();
        Self {
            started: now,
            state: Mutex::new(TrackerState {
                phase: LaunchPhase::Starting,
                phase_started: now,
                steps: Vec::new(),
                update: None,
                updated_rows: None,
                last_update_emitted: None,
                emitter: None,
            }),
        }
    }
}

impl LaunchTracker {
    pub(crate) fn set_emitter(&self, emitter: Emitter) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).emitter = Some(emitter);
    }

    fn snapshot_at(&self, state: &TrackerState, now: Instant) -> LaunchProgress {
        LaunchProgress {
            phase: state.phase,
            elapsed_ms: millis(now.duration_since(self.started)),
            steps: state.steps.clone(),
            update: state.update.clone(),
        }
    }

    pub(crate) fn snapshot(&self) -> LaunchProgress {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        self.snapshot_at(&state, Instant::now())
    }

    pub(crate) fn enter(&self, phase: LaunchPhase) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.phase == LaunchPhase::Ready || state.phase == phase {
            return;
        }
        let now = Instant::now();
        let previous = state.phase;
        let duration_ms = millis(now.duration_since(state.phase_started));
        state.steps.push(LaunchStep {
            phase: previous,
            duration_ms,
        });
        state.phase = phase;
        state.phase_started = now;
        if phase == LaunchPhase::Ready {
            eprintln!("{}", summary(&state.steps, state.updated_rows, millis(now.duration_since(self.started))));
        }
        if let Some(emitter) = &state.emitter {
            emitter(self.snapshot_at(&state, now));
        }
    }

    pub(crate) fn mark_ready(&self) {
        self.enter(LaunchPhase::Ready);
    }

    pub(crate) fn set_update(&self, completed: u64, total: u64) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        state.update = Some(LaunchUpdate { completed, total });
        state.updated_rows = Some(completed);
        if completed >= total
            || state
                .last_update_emitted
                .is_none_or(|last| now.duration_since(last) >= UPDATE_INTERVAL)
        {
            state.last_update_emitted = Some(now);
            if let Some(emitter) = &state.emitter {
                emitter(self.snapshot_at(&state, now));
            }
        }
    }

    pub(crate) fn clear_update(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.update.take().is_some() {
            state.last_update_emitted = None;
            if let Some(emitter) = &state.emitter {
                emitter(self.snapshot_at(&state, Instant::now()));
            }
        }
    }
}

fn summary(steps: &[LaunchStep], updated_rows: Option<u64>, total_ms: u64) -> String {
    let parts: Vec<_> = steps.iter().map(|step| {
        let rows = if step.phase == LaunchPhase::Updating {
            updated_rows.map(|rows| format!(" ({rows} rows)")).unwrap_or_default()
        } else {
            String::new()
        };
        format!("{} {}ms{}", phase_name(step.phase), step.duration_ms, rows)
    }).collect();
    format!("peppy startup: {}, total {}ms", parts.join(", "), total_ms)
}

fn millis(duration: Duration) -> u64 {
    duration.as_millis().min(u64::MAX as u128) as u64
}

fn phase_name(phase: LaunchPhase) -> &'static str {
    match phase {
        LaunchPhase::Starting => "starting",
        LaunchPhase::Unlocking => "unlocking",
        LaunchPhase::Opening => "opening",
        LaunchPhase::Updating => "updating",
        LaunchPhase::Loading => "loading",
        LaunchPhase::Ready => "ready",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_includes_updated_rows() {
        let steps = [LaunchStep { phase: LaunchPhase::Updating, duration_ms: 42 }];
        assert_eq!(summary(&steps, Some(48000), 50), "peppy startup: updating 42ms (48000 rows), total 50ms");
    }

    #[test]
    fn command_is_registered_for_main_and_composer() {
        assert!(include_str!("../build.rs").contains("\"launch_progress\","));
        assert!(include_str!("lib.rs").contains("        launch_progress,"));
        assert!(include_str!("../capabilities/default.json").contains("\"allow-launch-progress\""));
        assert!(include_str!("../capabilities/composer.json").contains("\"allow-launch-progress\""));
        assert!(crate::COMPOSER_COMMANDS.contains(&"launch_progress"));
    }

    #[test]
    fn records_steps_and_never_regresses_after_ready() {
        let tracker = LaunchTracker::default();
        tracker.enter(LaunchPhase::Unlocking);
        std::thread::sleep(Duration::from_millis(2));
        tracker.enter(LaunchPhase::Opening);
        tracker.mark_ready();
        tracker.enter(LaunchPhase::Loading);
        let progress = tracker.snapshot();
        assert_eq!(progress.phase, LaunchPhase::Ready);
        assert_eq!(
            progress
                .steps
                .iter()
                .map(|step| step.phase)
                .collect::<Vec<_>>(),
            vec![
                LaunchPhase::Starting,
                LaunchPhase::Unlocking,
                LaunchPhase::Opening
            ]
        );
        assert!(progress.steps[1].duration_ms >= 2);
    }

    #[test]
    fn throttles_updates_but_emits_final_and_clear() {
        let tracker = LaunchTracker::default();
        let events = Arc::new(Mutex::new(Vec::new()));
        let recorded = events.clone();
        tracker.set_emitter(Arc::new(move |event| recorded.lock().unwrap().push(event)));
        tracker.set_update(1, 3);
        tracker.set_update(2, 3);
        tracker.set_update(3, 3);
        tracker.clear_update();
        let events = events.lock().unwrap();
        assert_eq!(events.len(), 3);
        assert_eq!(events[1].update.as_ref().unwrap().completed, 3);
        assert!(events[2].update.is_none());
        assert!(serde_json::to_value(&events[0])
            .unwrap()
            .get("update")
            .is_some());
        assert!(serde_json::to_value(&events[2])
            .unwrap()
            .get("update")
            .is_none());
    }
}
