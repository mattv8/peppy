use peppy_desktop_api::{LaunchPhase, LaunchProgress, LaunchStep};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

pub(crate) const EVENT: &str = "peppy://launch-progress";
type Emitter = Arc<dyn Fn(LaunchProgress) + Send + Sync>;

pub(crate) struct LaunchTracker {
    started: Instant,
    state: Mutex<TrackerState>,
}

struct TrackerState {
    phase: LaunchPhase,
    phase_started: Instant,
    steps: Vec<LaunchStep>,
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
            update: None,
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
            eprintln!(
                "{}",
                summary(&state.steps, millis(now.duration_since(self.started)))
            );
        }
        if let Some(emitter) = &state.emitter {
            emitter(self.snapshot_at(&state, now));
        }
    }

    pub(crate) fn mark_ready(&self) {
        self.enter(LaunchPhase::Ready);
    }
}

fn summary(steps: &[LaunchStep], total_ms: u64) -> String {
    let parts: Vec<_> = steps
        .iter()
        .map(|step| format!("{} {}ms", phase_name(step.phase), step.duration_ms))
        .collect();
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
}
