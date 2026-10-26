use std::{
    fs,
    io::{self, ErrorKind},
    path::{Path, PathBuf},
};

const PLACEMENT_FILE: &str = "main-window-placement.json";
const MAX_FILE_BYTES: u64 = 4096;
const MAX_DIMENSION: u32 = 100_000;
const MIN_SCALE_FACTOR: f64 = 0.1;
const MAX_SCALE_FACTOR: f64 = 16.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Placement {
    pub bounds: Rect,
    pub scale_factor: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Monitor {
    pub work_area: Rect,
    pub scale_factor: f64,
}

pub(crate) fn fit_to_monitors(placement: Placement, monitors: &[Monitor]) -> Option<Placement> {
    if !is_valid_placement(placement) {
        return None;
    }

    let monitor = select_monitor(placement.bounds, monitors)?;

    let width = scaled_dimension(
        placement.bounds.width,
        placement.scale_factor,
        monitor.scale_factor,
    )
    .min(monitor.work_area.width)
    .min(MAX_DIMENSION);
    let height = scaled_dimension(
        placement.bounds.height,
        placement.scale_factor,
        monitor.scale_factor,
    )
    .min(monitor.work_area.height)
    .min(MAX_DIMENSION);

    Some(Placement {
        bounds: Rect {
            x: clamp_position(
                placement.bounds.x,
                monitor.work_area.x,
                monitor.work_area.width,
                width,
            ),
            y: clamp_position(
                placement.bounds.y,
                monitor.work_area.y,
                monitor.work_area.height,
                height,
            ),
            width,
            height,
        },
        scale_factor: monitor.scale_factor,
    })
}

pub(crate) struct Store {
    path: PathBuf,
}

impl Store {
    pub(crate) fn new(root: &Path) -> Self {
        Self {
            path: root.join(PLACEMENT_FILE),
        }
    }

    pub(crate) fn load(&self) -> std::io::Result<Option<Placement>> {
        let metadata = match fs::metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        if !metadata.is_file() || metadata.len() > MAX_FILE_BYTES {
            return Ok(None);
        }

        let bytes = fs::read(&self.path)?;
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return Ok(None);
        }
        let file: StoredPlacement = match serde_json::from_slice(&bytes) {
            Ok(file) => file,
            Err(_) => return Ok(None),
        };
        Ok((file.version == 1 && is_valid_placement(file.placement)).then_some(file.placement))
    }

    pub(crate) fn save(&self, placement: Placement) -> std::io::Result<()> {
        if !is_valid_placement(placement) {
            return Err(io::Error::new(
                ErrorKind::InvalidInput,
                "invalid window placement",
            ));
        }
        let bytes = serde_json::to_vec(&StoredPlacement {
            version: 1,
            placement,
        })
        .map_err(io::Error::other)?;
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return Err(io::Error::new(
                ErrorKind::InvalidData,
                "window placement is too large",
            ));
        }
        crate::fsutil::write_private_atomic(&self.path, &bytes)
    }
}

#[derive(serde::Deserialize, serde::Serialize)]
struct StoredPlacement {
    version: u8,
    placement: Placement,
}

fn is_valid_placement(placement: Placement) -> bool {
    placement.scale_factor.is_finite()
        && (MIN_SCALE_FACTOR..=MAX_SCALE_FACTOR).contains(&placement.scale_factor)
        && (1..=MAX_DIMENSION).contains(&placement.bounds.width)
        && (1..=MAX_DIMENSION).contains(&placement.bounds.height)
}

fn is_usable_monitor(monitor: Monitor) -> bool {
    monitor.scale_factor.is_finite()
        && (MIN_SCALE_FACTOR..=MAX_SCALE_FACTOR).contains(&monitor.scale_factor)
        && monitor.work_area.width > 0
        && monitor.work_area.height > 0
}

fn select_monitor(bounds: Rect, monitors: &[Monitor]) -> Option<&Monitor> {
    let mut usable = monitors
        .iter()
        .filter(|monitor| is_usable_monitor(**monitor));
    let mut selected = usable.next()?;
    let mut largest_overlap = intersection_area(bounds, selected.work_area);
    for monitor in usable {
        let overlap = intersection_area(bounds, monitor.work_area);
        if overlap > largest_overlap {
            selected = monitor;
            largest_overlap = overlap;
        }
    }
    if largest_overlap > 0 {
        return Some(selected);
    }

    let mut usable = monitors
        .iter()
        .filter(|monitor| is_usable_monitor(**monitor));
    let mut nearest = usable.next()?;
    let mut shortest_distance = distance_to_rect(bounds, nearest.work_area);
    for monitor in usable {
        let distance = distance_to_rect(bounds, monitor.work_area);
        if distance < shortest_distance {
            nearest = monitor;
            shortest_distance = distance;
        }
    }
    Some(nearest)
}

fn scaled_dimension(value: u32, source_scale: f64, target_scale: f64) -> u32 {
    let scaled = (f64::from(value) * target_scale / source_scale).round();
    scaled.clamp(1.0, f64::from(u32::MAX)) as u32
}

fn intersection_area(first: Rect, second: Rect) -> i128 {
    let width = overlap_length(first.x, first.width, second.x, second.width);
    let height = overlap_length(first.y, first.height, second.y, second.height);
    i128::from(width) * i128::from(height)
}

fn overlap_length(
    first_position: i32,
    first_length: u32,
    second_position: i32,
    second_length: u32,
) -> i64 {
    let start = i64::from(first_position).max(i64::from(second_position));
    let end = rect_end(first_position, first_length).min(rect_end(second_position, second_length));
    (end - start).max(0)
}

fn distance_to_rect(first: Rect, second: Rect) -> i128 {
    let x = axis_distance(first.x, first.width, second.x, second.width);
    let y = axis_distance(first.y, first.height, second.y, second.height);
    i128::from(x) * i128::from(x) + i128::from(y) * i128::from(y)
}

fn axis_distance(
    first_position: i32,
    first_length: u32,
    second_position: i32,
    second_length: u32,
) -> i64 {
    let first_end = rect_end(first_position, first_length);
    let second_end = rect_end(second_position, second_length);
    if first_end < i64::from(second_position) {
        i64::from(second_position) - first_end
    } else if second_end < i64::from(first_position) {
        i64::from(first_position) - second_end
    } else {
        0
    }
}

fn rect_end(position: i32, length: u32) -> i64 {
    i64::from(position) + i64::from(length)
}

fn clamp_position(position: i32, area_position: i32, area_length: u32, length: u32) -> i32 {
    let start = i64::from(area_position);
    let end = rect_end(area_position, area_length) - i64::from(length);
    i64::from(position).clamp(start, end) as i32
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

    fn monitor(work_area: Rect, scale_factor: f64) -> Monitor {
        Monitor {
            work_area,
            scale_factor,
        }
    }

    fn placement(bounds: Rect, scale_factor: f64) -> Placement {
        Placement {
            bounds,
            scale_factor,
        }
    }

    #[test]
    fn retains_a_frame_already_inside_its_work_area() {
        let placement = placement(rect(-1200, 20, 800, 600), 1.0);
        assert_eq!(
            fit_to_monitors(placement, &[monitor(rect(-1920, 0, 1920, 1040), 1.0)]),
            Some(placement)
        );
    }

    #[test]
    fn contains_frames_off_every_work_area_edge() {
        let monitor = monitor(rect(0, 0, 1000, 800), 1.0);
        for (input, expected) in [
            (rect(-200, 20, 400, 300), rect(0, 20, 400, 300)),
            (rect(800, 20, 400, 300), rect(600, 20, 400, 300)),
            (rect(20, -200, 400, 300), rect(20, 0, 400, 300)),
            (rect(20, 700, 400, 300), rect(20, 500, 400, 300)),
        ] {
            assert_eq!(
                fit_to_monitors(placement(input, 1.0), &[monitor]),
                Some(placement(expected, 1.0))
            );
        }
    }

    #[test]
    fn chooses_nearest_monitor_when_the_saved_frame_is_in_a_gap() {
        let monitors = [
            monitor(rect(-1600, 0, 1600, 900), 1.0),
            monitor(rect(400, 0, 1600, 900), 1.0),
        ];
        assert_eq!(
            fit_to_monitors(placement(rect(200, 40, 100, 100), 1.0), &monitors),
            Some(placement(rect(400, 40, 100, 100), 1.0))
        );
    }

    #[test]
    fn uses_the_work_area_not_the_full_display() {
        let monitor = monitor(rect(0, 24, 1440, 876), 1.0);
        assert_eq!(
            fit_to_monitors(placement(rect(20, 850, 800, 300), 1.0), &[monitor]),
            Some(placement(rect(20, 600, 800, 300), 1.0))
        );
    }

    #[test]
    fn preserves_logical_size_when_moving_to_a_different_scale() {
        let monitors = [
            monitor(rect(0, 0, 1000, 800), 1.0),
            monitor(rect(1000, 0, 2000, 1600), 2.0),
        ];
        assert_eq!(
            fit_to_monitors(placement(rect(1100, 100, 600, 400), 1.0), &monitors),
            Some(placement(rect(1100, 100, 1200, 800), 2.0))
        );
    }

    #[test]
    fn shrinks_an_oversized_frame_to_the_available_area() {
        assert_eq!(
            fit_to_monitors(
                placement(rect(-50, -50, 2000, 1500), 1.0),
                &[monitor(rect(-100, -100, 800, 500), 1.0)],
            ),
            Some(placement(rect(-100, -100, 800, 500), 1.0))
        );
    }

    #[test]
    fn selects_input_order_for_overlap_and_distance_ties() {
        let overlapping_monitors = [
            monitor(rect(0, 0, 100, 100), 1.0),
            monitor(rect(100, 0, 100, 100), 2.0),
        ];
        assert_eq!(
            fit_to_monitors(placement(rect(50, 0, 100, 100), 1.0), &overlapping_monitors),
            Some(placement(rect(0, 0, 100, 100), 1.0))
        );
        let separated_monitors = [
            monitor(rect(0, 0, 100, 100), 1.0),
            monitor(rect(200, 0, 100, 100), 2.0),
        ];
        assert_eq!(
            fit_to_monitors(placement(rect(145, 0, 10, 10), 1.0), &separated_monitors),
            Some(placement(rect(90, 0, 10, 10), 1.0))
        );
    }

    #[test]
    fn rejects_invalid_placements_and_unusable_monitors() {
        let valid_monitor = monitor(rect(0, 0, 100, 100), 1.0);
        for invalid in [
            placement(rect(0, 0, 0, 10), 1.0),
            placement(rect(0, 0, 100_001, 10), 1.0),
            placement(rect(0, 0, 10, 10), f64::NAN),
            placement(rect(0, 0, 10, 10), 16.1),
        ] {
            assert_eq!(fit_to_monitors(invalid, &[valid_monitor]), None);
        }
        assert_eq!(
            fit_to_monitors(placement(rect(0, 0, 10, 10), 1.0), &[]),
            None
        );
        assert_eq!(
            fit_to_monitors(
                placement(rect(0, 0, 10, 10), 1.0),
                &[monitor(rect(0, 0, 0, 100), 1.0)],
            ),
            None
        );
    }

    #[test]
    fn handles_extreme_coordinates_and_is_idempotent() {
        let monitors = [monitor(rect(i32::MIN, i32::MIN, u32::MAX, u32::MAX), 16.0)];
        let fitted = fit_to_monitors(
            placement(rect(i32::MAX, i32::MAX, 100_000, 100_000), 0.1),
            &monitors,
        )
        .unwrap();
        assert!(fitted.bounds.width <= MAX_DIMENSION && fitted.bounds.height <= MAX_DIMENSION);
        assert_eq!(fit_to_monitors(fitted, &monitors), Some(fitted));
    }

    #[test]
    fn persists_a_placement_across_store_instances() {
        let directory = tempfile::tempdir().unwrap();
        let expected = placement(rect(-1200, 80, 900, 700), 1.25);

        Store::new(directory.path()).save(expected).unwrap();

        assert_eq!(Store::new(directory.path()).load().unwrap(), Some(expected));
    }

    #[test]
    fn ignores_invalid_version_corrupt_and_oversized_state() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(PLACEMENT_FILE);
        let store = Store::new(directory.path());

        for contents in [
            b"not json".as_slice(),
            br#"{"version":2,"placement":{"bounds":{"x":0,"y":0,"width":1,"height":1},"scale_factor":1}}"#,
        ] {
            fs::write(&path, contents).unwrap();
            assert_eq!(store.load().unwrap(), None);
        }
        fs::write(&path, vec![b'x'; MAX_FILE_BYTES as usize + 1]).unwrap();
        assert_eq!(store.load().unwrap(), None);
    }

    #[test]
    fn refuses_to_save_invalid_placements() {
        let directory = tempfile::tempdir().unwrap();
        assert_eq!(
            Store::new(directory.path())
                .save(placement(rect(0, 0, 10, 10), 0.0))
                .unwrap_err()
                .kind(),
            ErrorKind::InvalidInput
        );
    }

    #[test]
    fn cleans_up_atomic_write_temporary_after_a_failed_publish() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join(PLACEMENT_FILE)).unwrap();

        assert!(Store::new(directory.path())
            .save(placement(rect(0, 0, 10, 10), 1.0))
            .is_err());

        let entries: Vec<_> = fs::read_dir(directory.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(entries, [std::ffi::OsString::from(PLACEMENT_FILE)]);
    }
}
