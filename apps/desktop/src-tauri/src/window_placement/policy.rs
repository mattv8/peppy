//! Platform-independent decisions used by the native placement controller.
use std::time::{Duration, Instant};

use crate::window_geometry::{fit_to_monitors, Monitor, Placement, Rect};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct Insets {
    pub width: f64,
    pub height: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Target {
    pub placement: Placement,
    pub work_area: Rect,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Sizing {
    pub content: (f64, f64),
    pub minimum: (f64, f64),
}

pub(super) fn window_bounds(bounds: Rect, scale: f64, logical_desktop: bool) -> Placement {
    if !logical_desktop {
        return Placement {
            bounds,
            scale_factor: scale,
        };
    }
    Placement {
        bounds: Rect {
            x: (f64::from(bounds.x) / scale).round() as i32,
            y: (f64::from(bounds.y) / scale).round() as i32,
            width: (f64::from(bounds.width) / scale).round() as u32,
            height: (f64::from(bounds.height) / scale).round() as u32,
        },
        scale_factor: 1.0,
    }
}

pub(super) fn monitor_bounds(area: Rect, scale: f64, logical_desktop: bool) -> Option<Monitor> {
    if !scale.is_finite() || !(0.1..=16.0).contains(&scale) || area.width == 0 || area.height == 0 {
        return None;
    }
    if !logical_desktop {
        return Some(Monitor {
            work_area: area,
            scale_factor: scale,
        });
    }
    let left = (f64::from(area.x) / scale).ceil() as i32;
    let top = (f64::from(area.y) / scale).ceil() as i32;
    let right = ((f64::from(area.x) + f64::from(area.width)) / scale).floor() as i64;
    let bottom = ((f64::from(area.y) + f64::from(area.height)) / scale).floor() as i64;
    let width = u32::try_from(right - i64::from(left)).ok()?;
    let height = u32::try_from(bottom - i64::from(top)).ok()?;
    (width > 0 && height > 0).then_some(Monitor {
        work_area: Rect {
            x: left,
            y: top,
            width,
            height,
        },
        scale_factor: 1.0,
    })
}

pub(super) fn plan(
    current: Placement,
    monitors: &[Monitor],
    minimum: (f64, f64),
    insets: Insets,
) -> Option<Target> {
    let mut fitted = fit_to_monitors(current, monitors)?;
    let monitor = monitors.iter().find(|monitor| {
        monitor.scale_factor == fitted.scale_factor && contains(monitor.work_area, fitted.bounds)
    })?;
    let min_width = ((minimum.0 + insets.width) * fitted.scale_factor).ceil() as u32;
    let min_height = ((minimum.1 + insets.height) * fitted.scale_factor).ceil() as u32;
    fitted.bounds.width = fitted
        .bounds
        .width
        .max(min_width.min(monitor.work_area.width));
    fitted.bounds.height = fitted
        .bounds
        .height
        .max(min_height.min(monitor.work_area.height));
    Some(Target {
        placement: fit_to_monitors(fitted, &[*monitor])?,
        work_area: monitor.work_area,
    })
}

fn contains(area: Rect, bounds: Rect) -> bool {
    bounds.x >= area.x
        && bounds.y >= area.y
        && i64::from(bounds.x) + i64::from(bounds.width)
            <= i64::from(area.x) + i64::from(area.width)
        && i64::from(bounds.y) + i64::from(bounds.height)
            <= i64::from(area.y) + i64::from(area.height)
}

pub(super) fn sizing(target: Target, minimum: (f64, f64), insets: Insets) -> Sizing {
    let scale = target.placement.scale_factor;
    let available = (
        (f64::from(target.work_area.width) / scale - insets.width).max(1.0),
        (f64::from(target.work_area.height) / scale - insets.height).max(1.0),
    );
    Sizing {
        content: (
            (f64::from(target.placement.bounds.width) / scale - insets.width).max(1.0),
            (f64::from(target.placement.bounds.height) / scale - insets.height).max(1.0),
        ),
        minimum: (minimum.0.min(available.0), minimum.1.min(available.1)),
    }
}

pub(super) fn minimum_growth(
    current: Placement,
    target: Target,
    minimum: (f64, f64),
    insets: Insets,
) -> bool {
    let required = sizing(target, minimum, insets).minimum;
    f64::from(current.bounds.width) / current.scale_factor - insets.width < required.0
        || f64::from(current.bounds.height) / current.scale_factor - insets.height < required.1
}

pub(super) fn preferred_save(
    initial: Option<Placement>,
    pending: Option<Placement>,
    current: Option<Placement>,
    cached: Option<Placement>,
) -> Option<Placement> {
    pending.or(current).or(cached).or(initial)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Action {
    Apply(Target),
    Check,
    Idle,
    Exhausted,
}

#[derive(Default)]
pub(super) struct Recovery {
    target: Option<(Target, u8)>,
    running: bool,
    check_requested: bool,
    topology: Option<Vec<Monitor>>,
    was_managed: bool,
}

impl Recovery {
    // The caller makes the first application; one deferred correction remains.
    pub fn begin(&mut self, target: Target) -> bool {
        self.target = Some((target, 1));
        self.start()
    }
    pub fn request_check(&mut self) -> bool {
        self.check_requested = true;
        self.start()
    }
    pub fn observe_environment(&mut self, topology: Option<&[Monitor]>, normal: bool) -> bool {
        let changed = topology.is_some_and(|current| {
            let changed = self.topology.as_deref() != Some(current);
            self.topology = Some(current.to_vec());
            changed
        });
        let returned = self.was_managed && normal;
        self.was_managed = !normal;
        if changed || returned {
            self.request_check()
        } else {
            false
        }
    }
    pub fn prime(&mut self, topology: Option<&[Monitor]>, normal: bool) {
        self.topology = topology.map(<[Monitor]>::to_vec);
        self.was_managed = !normal;
    }
    fn start(&mut self) -> bool {
        if self.running {
            false
        } else {
            self.running = true;
            true
        }
    }
    pub fn target(&self) -> Option<Target> {
        self.target.map(|(target, _)| target)
    }
    pub fn next(&mut self, actual: Option<Placement>, normal: bool) -> Action {
        if !normal {
            self.target = None;
            self.check_requested = false;
        }
        if let Some((target, remaining)) = self.target.as_mut() {
            if actual != Some(target.placement) {
                if *remaining > 0 {
                    *remaining -= 1;
                    return Action::Apply(*target);
                }
                self.target = None;
                return Action::Exhausted;
            }
            self.target = None;
        }
        if self.check_requested {
            self.check_requested = false;
            return Action::Check;
        }
        self.running = false;
        Action::Idle
    }
}

#[derive(Default)]
pub(super) struct SaveQueue {
    pending: Option<(Instant, u64)>,
    running: bool,
}

impl SaveQueue {
    pub fn request(&mut self, now: Instant, generation: u64) -> bool {
        self.pending = Some((now + Duration::from_millis(400), generation));
        if self.running {
            false
        } else {
            self.running = true;
            true
        }
    }
    pub fn deadline_or_finish(&mut self) -> Option<Instant> {
        if self.pending.is_none() {
            self.running = false;
        }
        self.pending.map(|(deadline, _)| deadline)
    }
    pub fn take_due(&mut self, now: Instant) -> Option<u64> {
        let (deadline, generation) = self.pending?;
        if deadline > now {
            return None;
        }
        self.pending = None;
        Some(generation)
    }
    pub fn cancel(&mut self) {
        self.pending = None;
    }
}

#[cfg(any(target_os = "linux", test))]
pub(super) fn position_supported(
    backend: Option<&str>,
    session: Option<&str>,
    wayland_display: bool,
) -> bool {
    match backend {
        Some("x11") => true,
        Some("wayland") => false,
        _ => {
            !wayland_display
                && !session.is_some_and(|session| session.eq_ignore_ascii_case("wayland"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn rect(x: i32, y: i32, width: u32, height: u32) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }
    fn placement(bounds: Rect, scale_factor: f64) -> Placement {
        Placement {
            bounds,
            scale_factor,
        }
    }
    fn target() -> Target {
        Target {
            placement: placement(rect(1600, 100, 1100, 760), 1.0),
            work_area: rect(1512, 30, 1920, 1050),
        }
    }

    #[test]
    fn mixed_dpi_mac_rectangles_share_global_points_on_every_test_platform() {
        let laptop = monitor_bounds(rect(0, 60, 3024, 1904), 2.0, true).unwrap();
        let external = monitor_bounds(rect(1512, 30, 1920, 1050), 1.0, true).unwrap();
        assert_eq!(laptop.work_area, rect(0, 30, 1512, 952));
        let current = window_bounds(rect(1600, 100, 1100, 760), 1.0, true);
        assert_eq!(
            plan(
                current,
                &[laptop, external],
                (760.0, 560.0),
                Insets::default()
            )
            .unwrap()
            .placement,
            current
        );
    }

    #[test]
    fn logical_desktop_capture_uses_the_window_scale_including_negative_origins() {
        let left = monitor_bounds(rect(-3840, 60, 3840, 2100), 2.0, true).unwrap();
        let right = monitor_bounds(rect(0, 30, 1920, 1050), 1.0, true).unwrap();
        let current = window_bounds(rect(-3200, 200, 2200, 1520), 2.0, true);
        assert_eq!(current, placement(rect(-1600, 100, 1100, 760), 1.0));
        assert_eq!(
            plan(current, &[left, right], (760.0, 560.0), Insets::default())
                .unwrap()
                .placement,
            current
        );
    }

    #[test]
    fn windows_physical_desktop_coordinates_are_not_normalized_twice() {
        let monitor = monitor_bounds(rect(1920, 30, 3840, 2100), 2.0, false).unwrap();
        let current = window_bounds(rect(2000, 100, 2200, 1520), 2.0, false);
        assert_eq!(current, placement(rect(2000, 100, 2200, 1520), 2.0));
        assert_eq!(
            plan(current, &[monitor], (760.0, 560.0), Insets::default())
                .unwrap()
                .placement,
            current
        );
    }

    #[test]
    fn small_display_relaxes_each_constraint_and_larger_display_restores_it() {
        let small = Monitor {
            work_area: rect(0, 30, 640, 450),
            scale_factor: 1.0,
        };
        let current = placement(rect(500, 200, 1100, 760), 1.0);
        let fitted = plan(current, &[small], (760.0, 560.0), Insets::default()).unwrap();
        assert_eq!(
            sizing(fitted, (760.0, 560.0), Insets::default()).minimum,
            (640.0, 450.0)
        );
        let large = Monitor {
            work_area: rect(0, 30, 1920, 1050),
            scale_factor: 1.0,
        };
        let fitted = plan(
            fitted.placement,
            &[large],
            (760.0, 560.0),
            Insets::default(),
        )
        .unwrap();
        assert_eq!(fitted.placement.bounds.width, 760);
        assert_eq!(fitted.placement.bounds.height, 560);
        assert_eq!(
            sizing(fitted, (760.0, 560.0), Insets::default()).minimum,
            (760.0, 560.0)
        );
    }

    #[test]
    fn frame_insets_and_target_scale_determine_native_logical_content_size() {
        let target = Target {
            placement: placement(rect(2000, 30, 1800, 1200), 2.0),
            work_area: rect(1920, 0, 2560, 1440),
        };
        let result = sizing(
            target,
            (760.0, 560.0),
            Insets {
                width: 8.0,
                height: 30.0,
            },
        );
        assert_eq!(result.content, (892.0, 570.0));
        assert_eq!(result.minimum, (760.0, 560.0));
    }

    #[test]
    fn deferred_native_mutation_retries_original_target_once_then_terminates() {
        let mut recovery = Recovery::default();
        let target = target();
        assert!(recovery.begin(target));
        assert!(!recovery.request_check());
        assert_eq!(recovery.target(), Some(target));
        let mutated = placement(rect(800, 50, 2200, 1520), 2.0);
        assert_eq!(recovery.next(Some(mutated), true), Action::Apply(target));
        assert_eq!(recovery.next(Some(mutated), true), Action::Exhausted);
        assert_eq!(recovery.target(), None);
    }

    #[test]
    fn managed_transition_cancels_pending_native_application() {
        let mut recovery = Recovery::default();
        recovery.begin(target());
        assert_eq!(recovery.next(None, false), Action::Idle);
        assert_eq!(recovery.target(), None);
    }

    #[test]
    fn empty_monitor_snapshot_does_not_propose_a_target() {
        assert!(plan(target().placement, &[], (760.0, 560.0), Insets::default()).is_none());
    }

    #[test]
    fn topology_and_managed_state_changes_are_latched_during_application() {
        let mut recovery = Recovery::default();
        let old = [Monitor {
            work_area: rect(0, 0, 1920, 1080),
            scale_factor: 1.0,
        }];
        let changed = [Monitor {
            work_area: rect(0, 30, 1920, 1050),
            scale_factor: 1.0,
        }];
        recovery.prime(Some(&old), true);
        assert!(!recovery.observe_environment(Some(&old), true));
        assert!(recovery.begin(target()));
        assert!(!recovery.observe_environment(Some(&changed), true));
        assert_eq!(recovery.next(Some(target().placement), true), Action::Check);
        assert_eq!(recovery.next(Some(target().placement), true), Action::Idle);
        assert!(!recovery.observe_environment(Some(&changed), false));
        assert!(recovery.observe_environment(Some(&changed), true));
        assert_eq!(recovery.next(None, true), Action::Check);
    }

    #[test]
    fn save_worker_stays_single_flight_through_cancel_and_new_user_move() {
        let mut saves = SaveQueue::default();
        let now = Instant::now();
        assert!(saves.request(now, 1));
        saves.cancel();
        assert!(!saves.request(now, 2));
        assert_eq!(saves.take_due(now + Duration::from_secs(1)), Some(2));
        assert_eq!(saves.deadline_or_finish(), None);
        assert!(saves.request(now, 3));
        assert_eq!(saves.take_due(now), None);
        assert!(saves.deadline_or_finish().is_some());
    }

    #[test]
    fn wayland_sessions_and_forced_x11_backend_are_distinguished() {
        assert!(!position_supported(None, Some("wayland"), true));
        assert!(!position_supported(Some("wayland"), None, false));
        assert!(position_supported(Some("x11"), Some("wayland"), true));
        assert!(position_supported(None, Some("x11"), false));
    }

    #[test]
    fn cross_dpi_motion_is_not_mistaken_for_minimum_size_growth() {
        let current = placement(rect(1800, 100, 1100, 760), 1.0);
        let destination = Target {
            placement: placement(rect(1920, 100, 2200, 1520), 2.0),
            work_area: rect(1920, 0, 3840, 2160),
        };
        assert!(!minimum_growth(
            current,
            destination,
            (760.0, 560.0),
            Insets::default()
        ));
        let too_small = placement(rect(1920, 100, 640, 450), 1.0);
        assert!(minimum_growth(
            too_small,
            destination,
            (760.0, 560.0),
            Insets::default()
        ));
    }

    #[test]
    fn unused_startup_state_cannot_replace_newer_live_or_cached_user_geometry() {
        let old = placement(rect(2500, 100, 900, 650), 1.0);
        let latest = placement(rect(100, 100, 900, 650), 1.0);
        assert_eq!(
            preferred_save(Some(old), None, Some(latest), None),
            Some(latest)
        );
        assert_eq!(
            preferred_save(Some(old), None, None, Some(latest)),
            Some(latest)
        );
        assert_eq!(
            preferred_save(Some(old), Some(latest), None, None),
            Some(latest)
        );
        assert_eq!(preferred_save(Some(old), None, None, None), Some(old));
    }

    #[test]
    fn a_window_scale_event_with_unchanged_display_layout_does_not_snap_a_drag() {
        let layout = [
            Monitor {
                work_area: rect(0, 0, 1920, 1080),
                scale_factor: 1.0,
            },
            Monitor {
                work_area: rect(1920, 0, 3840, 2160),
                scale_factor: 2.0,
            },
        ];
        let mut recovery = Recovery::default();
        recovery.prime(Some(&layout), true);
        // Moving the window changes its own DPI, not the monitors' work areas/scales.
        assert!(!recovery.observe_environment(Some(&layout), true));
        assert_eq!(recovery.next(None, true), Action::Idle);
    }
}
