//! A Slint model whose rows are built on demand.

use std::cell::Cell;

use slint::{Model, ModelNotify, ModelTracker};

/// Builds each row only when the list asks for it, so a long playlist costs
/// nothing for the rows that are never scrolled into view.
pub struct LazyModel<T> {
    len: Cell<usize>,
    build: Box<dyn Fn(usize) -> Option<T>>,
    notify: ModelNotify,
}

impl<T> LazyModel<T> {
    pub fn new(build: impl Fn(usize) -> Option<T> + 'static) -> Self {
        Self {
            len: Cell::new(0),
            build: Box::new(build),
            notify: ModelNotify::default(),
        }
    }

    /// Replaces the contents with `len` rows.
    pub fn reset(&self, len: usize) {
        self.len.set(len);
        self.notify.reset();
    }

    /// Rebuilds one row.
    pub fn refresh(&self, row: usize) {
        if row < self.len.get() {
            self.notify.row_changed(row);
        }
    }

    /// Rebuilds every row, keeping the list's scroll position.
    pub fn refresh_all(&self) {
        for row in 0..self.len.get() {
            self.notify.row_changed(row);
        }
    }
}

impl<T: 'static> Model for LazyModel<T> {
    type Data = T;

    fn row_count(&self) -> usize {
        self.len.get()
    }

    fn row_data(&self, row: usize) -> Option<T> {
        (row < self.len.get()).then(|| (self.build)(row)).flatten()
    }

    fn model_tracker(&self) -> &dyn ModelTracker {
        &self.notify
    }
}
