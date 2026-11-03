//! Per-window first-show lifecycle: windows show only after their first page load or an explicit retry.

#[derive(Default)]
pub struct Materials {
    ready: std::collections::HashSet<String>,
    waiting_to_show: std::collections::HashSet<String>,
}

impl Materials {
    pub fn forget(&mut self, label: &str) {
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
}
