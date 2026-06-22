use gtk::prelude::*;
use std::cell::Cell;
use std::rc::Rc;

/// Horizontal bar combining a live input-level fill with a draggable sensitivity threshold.
///
/// The filled portion below the threshold is orange (#f0a500); above the threshold it is green
/// (#3ba55d). A thin white handle marks the threshold position. The dBFS range is -60..0.
pub struct LevelBar {
    pub drawing_area: gtk::DrawingArea,
    /// Live input level, dBFS (-60..0).
    pub level_db: Rc<Cell<f32>>,
    /// Sensitivity threshold, dBFS (-60..0).
    pub threshold_db: Rc<Cell<f32>>,
}

impl LevelBar {
    /// `threshold_db`: initial threshold in dBFS.
    /// `on_threshold`: called with the new threshold whenever the user drags the handle.
    pub fn new(threshold_db_init: f32, on_threshold: impl Fn(f32) + 'static) -> Self {
        // Shared so both the press (click) and the drag-update path can report
        // the new threshold; a pure click only fires `drag-begin`.
        let on_threshold = Rc::new(on_threshold);

        let area = gtk::DrawingArea::builder()
            .height_request(24)
            .hexpand(true)
            .build();

        let level_db = Rc::new(Cell::new(-60.0f32));
        let threshold_db = Rc::new(Cell::new(threshold_db_init));

        // Draw function – captures shared state by Rc clone.
        {
            let level = level_db.clone();
            let thresh = threshold_db.clone();

            area.set_draw_func(move |_area, cr, width, height| {
                let w = width as f64;
                let h = height as f64;

                let db_to_x = |db: f32| -> f64 {
                    ((db + 60.0) / 60.0).clamp(0.0, 1.0) as f64 * w
                };

                let level_x = db_to_x(level.get());
                let thresh_x = db_to_x(thresh.get());

                // Dark track background.
                cr.set_source_rgb(0.15, 0.15, 0.15);
                cr.rectangle(0.0, 0.0, w, h);
                let _ = cr.fill();

                // Fill from left to level position.
                if level_x > 0.0 {
                    // Orange portion: left edge up to min(level_x, thresh_x).
                    let orange_end = level_x.min(thresh_x);
                    if orange_end > 0.0 {
                        cr.set_source_rgb(0.941, 0.647, 0.0); // #f0a500
                        cr.rectangle(0.0, 0.0, orange_end, h);
                        let _ = cr.fill();
                    }

                    // Green portion: thresh_x up to level_x (only when level > threshold).
                    if level_x > thresh_x {
                        cr.set_source_rgb(0.231, 0.647, 0.365); // #3ba55d
                        cr.rectangle(thresh_x, 0.0, level_x - thresh_x, h);
                        let _ = cr.fill();
                    }
                }

                // Threshold handle: a thin white vertical bar.
                let handle_w = 3.0_f64;
                cr.set_source_rgb(1.0, 1.0, 1.0);
                cr.rectangle((thresh_x - handle_w / 2.0).max(0.0), 0.0, handle_w, h);
                let _ = cr.fill();
            });
        }

        // Gesture for click + drag to set threshold.
        let drag = gtk::GestureDrag::new();
        drag.set_button(gtk::gdk::BUTTON_PRIMARY);

        {
            let thresh = threshold_db.clone();
            let area_ref = area.clone();

            // Helper: map pointer x to dBFS, clamp to -60..0.
            let x_to_db = {
                let area_ref = area_ref.clone();
                move |x: f64| -> f32 {
                    let w = area_ref.width() as f64;
                    if w <= 0.0 {
                        return -60.0;
                    }
                    let ratio = (x / w).clamp(0.0, 1.0);
                    (ratio as f32 * 60.0 - 60.0).clamp(-60.0, 0.0)
                }
            };

            // Track drag start so we can compute absolute position during update.
            let drag_start_x: Rc<Cell<f64>> = Rc::new(Cell::new(0.0));

            let drag_start = drag_start_x.clone();
            drag.connect_drag_begin({
                let thresh = thresh.clone();
                let area_redraw = area_ref.clone();
                let x_to_db = x_to_db.clone();
                let on_threshold_press = on_threshold.clone();
                // `x` here is the press position in widget coords (== start_point).
                move |_gesture, x, _y| {
                    drag_start.set(x);
                    let db = x_to_db(x);
                    thresh.set(db);
                    area_redraw.queue_draw();
                    on_threshold_press(db);
                }
            });

            drag.connect_drag_update({
                let thresh = thresh.clone();
                let area_redraw = area_ref.clone();
                let drag_start = drag_start_x.clone();
                let on_threshold_drag = on_threshold.clone();
                let x_to_db = x_to_db.clone();
                move |_gesture, offset_x, _offset_y| {
                    let abs_x = drag_start.get() + offset_x;
                    let db = x_to_db(abs_x);
                    thresh.set(db);
                    area_redraw.queue_draw();
                    on_threshold_drag(db);
                }
            });
        }

        area.add_controller(drag);

        Self {
            drawing_area: area,
            level_db,
            threshold_db,
        }
    }

    /// Update the live level fill. Queues a redraw.
    pub fn set_level(&self, db: f32) {
        self.level_db.set(db.clamp(-60.0, 0.0));
        self.drawing_area.queue_draw();
    }

    /// Set the threshold position without emitting (used during settings populate).
    pub fn set_threshold_silent(&self, db: f32) {
        self.threshold_db.set(db.clamp(-60.0, 0.0));
        self.drawing_area.queue_draw();
    }
}
