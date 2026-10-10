//! Per-window material lifecycle; native effects change only on explicit mode transitions.
use std::collections::HashMap;

#[derive(Default)]
pub struct Materials {
    windows: HashMap<String, (bool, bool)>,
    ready: std::collections::HashSet<String>,
    waiting_to_show: std::collections::HashSet<String>,
}

impl Materials {
    pub fn begin_configure(&mut self, label: &str, head_panel: bool) -> Option<bool> {
        if let Some((previous_panel, _)) = self.windows.get(label) {
            if *previous_panel == head_panel {
                return None;
            }
        }
        // Reserve the transition before AppKit calls can deliver another event.
        self.windows.insert(label.to_owned(), (head_panel, false));
        Some(!head_panel)
    }

    pub fn finish_configure(&mut self, label: &str, head_panel: bool, installed: bool) {
        if let Some((current_panel, active)) = self.windows.get_mut(label) {
            if *current_panel == head_panel {
                *active = installed && !head_panel;
            }
        }
    }

    pub fn active(&self, label: &str) -> bool {
        self.windows.get(label).is_some_and(|(_, active)| *active)
    }

    pub fn forget(&mut self, label: &str) {
        self.windows.remove(label);
        self.ready.remove(label);
        self.waiting_to_show.remove(label);
    }

    pub fn request_show(&mut self, label: &str) -> bool {
        if self.ready.contains(label) {
            return true;
        }
        // A second explicit tray/open request recovers from a navigation which
        // never emits Finished. Quiet startup never makes this request.
        if self.waiting_to_show.remove(label) {
            self.ready.insert(label.to_owned());
            return true;
        }
        self.waiting_to_show.insert(label.to_owned());
        false
    }

    pub fn page_loaded(&mut self, label: &str) -> bool {
        self.ready.insert(label.to_owned());
        self.waiting_to_show.remove(label)
    }
}

#[cfg(test)]
mod tests {
    use super::Materials;

    #[test]
    fn application_is_reserved_before_native_work_and_stale_results_are_ignored() {
        let mut materials = Materials::default();
        assert_eq!(materials.begin_configure("composer", false), Some(true));
        assert!(!materials.active("composer"));
        assert_eq!(materials.begin_configure("composer", false), None);
        assert_eq!(materials.begin_configure("composer", true), Some(false));
        materials.finish_configure("composer", false, true);
        assert!(!materials.active("composer"));
        materials.finish_configure("composer", true, false);
        assert_eq!(materials.begin_configure("composer", false), Some(true));
        materials.finish_configure("composer", false, true);
        assert!(materials.active("composer"));
    }

    #[test]
    fn explicit_retry_can_show_a_window_whose_first_page_never_finishes() {
        let mut materials = Materials::default();
        assert!(!materials.request_show("main"));
        assert!(materials.request_show("main"));
        assert!(
            !materials.page_loaded("main"),
            "late load must not steal focus"
        );
    }

    #[test]
    fn first_show_waits_for_loaded_page_but_background_start_stays_hidden() {
        let mut materials = Materials::default();
        assert!(!materials.request_show("main"));
        assert!(materials.page_loaded("main"));
        assert!(materials.request_show("main"));
        assert!(!materials.page_loaded("background"));
        assert!(materials.request_show("background"));
        materials.forget("main");
        assert!(!materials.request_show("main"));
    }

    #[test]
    fn reuses_material_until_panel_mode_changes() {
        let mut materials = Materials::default();
        assert_eq!(materials.begin_configure("composer", false), Some(true));
        materials.finish_configure("composer", false, true);
        assert_eq!(materials.begin_configure("composer", false), None);
        assert!(materials.active("composer"));
        assert_eq!(materials.begin_configure("composer", true), Some(false));
        materials.finish_configure("composer", true, false);
        assert!(!materials.active("composer"));
        assert_eq!(materials.begin_configure("composer", false), Some(true));
    }

    #[test]
    fn failure_stays_opaque_and_recreated_window_can_retry() {
        let mut materials = Materials::default();
        assert_eq!(materials.begin_configure("main", false), Some(true));
        materials.finish_configure("main", false, false);
        assert!(!materials.active("main"));
        assert_eq!(materials.begin_configure("main", false), None);
        materials.forget("main");
        assert!(!materials.active("main"));
        assert_eq!(materials.begin_configure("main", false), Some(true));
    }

    #[test]
    fn panel_state_cannot_leak_to_another_window() {
        let mut materials = Materials::default();
        materials.begin_configure("main", false);
        materials.finish_configure("main", false, true);
        materials.begin_configure("composer", true);
        materials.finish_configure("composer", true, false);
        assert!(materials.active("main"));
        assert!(!materials.active("composer"));
    }
}
