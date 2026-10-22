//! OWNER: data agent. Window icon extraction to premultiplied BGRA pixels, with caching.

use std::collections::HashSet;
use std::sync::Arc;

use crate::model::{Hwnd, IconImage};

pub struct IconCache {}

impl IconCache {
    pub fn new() -> Self {
        Self {}
    }

    /// Icon for a top-level window, ideally >= 32x32. Cached; must be cheap on cache hit.
    pub fn get(&mut self, hwnd: Hwnd) -> Option<Arc<IconImage>> {
        let _ = hwnd;
        None
    }

    /// Drop cache entries for windows that no longer exist.
    pub fn retain(&mut self, alive: &HashSet<Hwnd>) {
        let _ = alive;
    }
}
