//! Remembering where a file was left: the page at the top of the window and, for an editable
//! document, the caret, so opening the same file again picks up where the reader left off.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{WordApp, canvas, now_ms};

/// Where the reader left a file: the page at the top of the window and, for an editable
/// document, the caret. Restored when the same file is opened again, like Word's "pick up
/// where you left off". A PDF has no caret, so only its page and the offset into that page
/// are kept.
///
/// Distances from a page's top edge are in document points, so a saved position survives a
/// different zoom or window size. They are rounded to [`VIEW_TOP_STEP`] to keep the number of
/// preference writes down while scrolling.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ViewPosition {
    /// Zero-based page at the top of the viewport.
    pub page: usize,
    /// Points from that page's top edge to the top of the viewport.
    pub top: f32,
    /// The caret, as `caret.set`'s `pos`; `None` for a read-only PDF.
    pub caret: Option<Value>,
    /// Paragraphs in an editable document, or pages in a PDF. A file that changed since the
    /// position was saved isn't restored at a stale character offset.
    pub fingerprint: usize,
}

/// Most files whose reading position is kept, so `ui.json` stays small.
pub const MAX_VIEW_POSITIONS: usize = 200;
/// Longest file key read back from the preferences (`ui.json` is user-editable).
const MAX_VIEW_KEY: usize = 2048;
/// Sane bounds for a saved page, offset and content fingerprint.
const MAX_VIEW_PAGE: usize = 1_000_000;
const MAX_VIEW_TOP: f32 = 100_000.0;
const MAX_VIEW_FINGERPRINT: usize = 10_000_000;
/// The grid the offset from the page's top is rounded to, in document points.
const VIEW_TOP_STEP: f32 = 8.0;
/// How often a position changed only by scrolling or typing is captured (milliseconds). A
/// document switch, a window close and every command that replaces the file capture at once.
pub(crate) const VIEW_CAPTURE_MS: f64 = 1000.0;

/// Keep only sane, small reading positions from a preferences file. `options::lenient` has
/// already dropped fields of the wrong type; this bounds the numbers (a jump to page 1e9 is
/// clamped by the canvas anyway, but a NaN would poison every scroll calculation) and the size
/// of the map, and drops a caret that can't be parsed as a document position.
pub(crate) fn sanitize_positions(map: &mut BTreeMap<String, ViewPosition>) {
    map.retain(|key, p| {
        let caret_ok = p.caret.as_ref().is_none_or(|c| {
            serde_json::from_value::<wordcraft_engine::doc::Pos>(c.clone()).is_ok() && serde_json::to_string(c).is_ok_and(|s| s.len() <= 1024)
        });
        !key.is_empty()
            && key.len() <= MAX_VIEW_KEY
            && p.page <= MAX_VIEW_PAGE
            && p.top.is_finite()
            && p.top >= 0.0
            && p.top <= MAX_VIEW_TOP
            && p.fingerprint <= MAX_VIEW_FINGERPRINT
            && caret_ok
    });
    for p in map.values_mut() {
        p.top = (p.top / VIEW_TOP_STEP).round() * VIEW_TOP_STEP;
    }
    while map.len() > MAX_VIEW_POSITIONS {
        let Some(first) = map.keys().next().cloned() else { break };
        map.remove(&first);
    }
}

/// The key a file's reading position is kept under. On the web a file arrives as bytes and
/// `path` is only its name, which is the best identity the browser gives us.
pub(crate) fn position_key(path: &Path) -> Option<String> {
    let key = path.to_string_lossy().into_owned();
    (!key.is_empty() && key.len() <= MAX_VIEW_KEY).then_some(key)
}

impl WordApp {
    /// Remember where the reader left the open file. The page at the top of the window is kept
    /// for Word documents and PDFs alike; an editable document also keeps its caret. Called
    /// before the file is replaced, when the window closes, and about once a second while
    /// reading, so pure scrolling and typing are covered without a preference write per frame.
    pub fn capture_view_position(&mut self) {
        self.last_view_capture = now_ms();
        if !self.has_document() {
            return;
        }
        let Some(key) = self.session.path.as_deref().and_then(position_key) else { return };
        let pdf = self.session.pdf_view();
        let fingerprint = pdf.as_ref().map_or_else(|| self.session.doc.paragraph_count(), |v| v.page_count());
        let caret = if pdf.is_none() { serde_json::to_value(&self.session.sel.focus).ok() } else { None };
        // Page rects are last frame's. Until the new file has been painted they still describe
        // the file that was replaced, so keep its page out of the new file's record.
        let rects_current = self.canvas.page_rects_document == Some(self.session.document_id());
        let previous = self.ui.positions.get(&key);
        let (page, top) = match rects_current.then(|| canvas::position_at_scroll(&self.canvas, self.session.view.read_mode)).flatten() {
            Some((page, top)) => (page.min(MAX_VIEW_PAGE), (top / VIEW_TOP_STEP).round() * VIEW_TOP_STEP),
            None => previous.map_or((0, 0.0), |p| (p.page, p.top)),
        };
        let position = ViewPosition { page, top, caret, fingerprint };
        if previous == Some(&position) {
            return;
        }
        if self.ui.positions.len() >= MAX_VIEW_POSITIONS && !self.ui.positions.contains_key(&key) {
            let Some(oldest) = self.ui.positions.keys().next().cloned() else { return };
            self.ui.positions.remove(&oldest);
        }
        self.ui.positions.insert(key, position);
    }

    /// Put back the position saved for the file that just opened. Runs after the engine replaced
    /// the document, and only for `file.open`: a new document starts at its beginning.
    pub(crate) fn restore_view_position(&mut self) {
        let Some(key) = self.session.path.as_deref().and_then(position_key) else { return };
        let Some(saved) = self.ui.positions.get(&key).cloned() else { return };
        let pdf = self.session.pdf_view();
        let fingerprint = pdf.as_ref().map_or_else(|| self.session.doc.paragraph_count(), |v| v.page_count());
        // A file edited elsewhere since the last visit may not have the same text at the same
        // offsets; opening it at its beginning is better than dropping the reader into the
        // middle of a different document.
        if saved.fingerprint != fingerprint {
            return;
        }
        if pdf.is_none()
            && let Some(caret) = saved.caret.as_ref()
            && let Ok(pos) = serde_json::from_value::<wordcraft_engine::doc::Pos>(caret.clone())
        {
            let pos = self.session.doc.clamp(&pos);
            self.session.sel = wordcraft_engine::Selection::caret(pos);
            self.session.goal_x = None;
            self.session.column = None;
            self.session.column_mode = false;
        }
        // The canvas applies this on its first frame with the new file's page rects.
        self.canvas.restore_view = Some((saved.page.min(MAX_VIEW_PAGE), saved.top.clamp(0.0, MAX_VIEW_TOP)));
        self.canvas.scroll_to_caret = false;
        self.canvas.nav_to_caret = false;
        self.canvas.read_target_page = None;
    }
}
