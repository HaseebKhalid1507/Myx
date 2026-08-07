//! What the renderer hands back, and when it is worth drawing at all.

use crate::*;

/// Mouse hit rects, written by the renderer and read only by `handle_mouse`.
///
/// Output only: nothing here is threaded frame-to-frame, and every rect is
/// (re)set or cleared on the frame that draws the thing it belongs to. The one
/// exception is `scroll_len`, which is written with `scroll` but not cleared
/// alongside it — reading it is only sound because `handle_mouse` checks
/// `scroll` is `Some` first.
#[derive(Default)]
pub(crate) struct HitRects {
    /// Last-rendered progress-bar rect (for click-to-seek).
    pub(crate) bar: Option<Rect>,
    /// Last-rendered sidebar scrollbar track + item count (drag-to-scroll).
    pub(crate) scroll: Option<Rect>,
    pub(crate) scroll_len: usize,
    /// Last-rendered volume-meter bar region (click/drag to set volume).
    pub(crate) vol: Option<Rect>,
    /// View tabs in the header.
    pub(crate) tabs: Vec<(RightView, Rect)>,
    /// Library list viewport.
    pub(crate) lib: Option<Rect>,
    /// Rows in the currently visible actions menu.
    pub(crate) actions: Vec<Rect>,
}

/// Everything the renderer writes, kept out of `App` so every render function
/// can take `&App`.
#[derive(Default)]
pub(crate) struct FrameOut {
    pub(crate) hits: HitRects,
    /// Library viewport start row. Unlike `hits`, this is read-modify-write:
    /// the renderer feeds the previous frame's value into `scroll_offset` and
    /// stores the result back, which is what makes scrolling sticky. Owned by
    /// `run_ui` so it survives across frames.
    pub(crate) lib_offset: usize,
}

/// What the album art box owes the next frame.
///
/// `ratatui-image` puts the whole image in one cell's symbol and marks the rest
/// of the box `Skip`, which the diff never touches again — so leftovers stay,
/// and a re-encode is byte-identical for sixel and iTerm2 and gets discarded.
/// Blanking the box for one frame is the only change the diff will emit, and it
/// makes the image that follows one too.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ArtRepaint {
    /// The box holds what it should.
    Idle,
    /// Blank the box this frame.
    Wipe,
    /// Draw the art; the wipe has gone out.
    Draw,
}

impl ArtRepaint {
    pub(crate) fn advance(self) -> Self {
        match self {
            Self::Wipe => Self::Draw,
            _ => Self::Idle,
        }
    }
}

pub(crate) fn repaint_for_layout_change(
    previous: (RightView, bool),
    current: (RightView, bool),
) -> ArtRepaint {
    match (previous, current) {
        ((RightView::NowPlaying, _), (RightView::NowPlaying, _)) => ArtRepaint::Wipe,
        ((_, _), (RightView::NowPlaying, _)) => ArtRepaint::Draw,
        _ => ArtRepaint::Idle,
    }
}

/// Ceiling on the redraw rate: one frame per ~60Hz terminal refresh.
pub(crate) const MIN_FRAME: Duration = Duration::from_millis(16);
/// Redraw rate while the visualizer or a theme fade is running.
pub(crate) const ANIM_FRAME: Duration = Duration::from_millis(33);
/// Redraw rate when nothing changed — enough to keep the clock and progress bar
/// honest without repainting an identical frame 60 times a second.
pub(crate) const IDLE_REDRAW: Duration = Duration::from_millis(500);
/// How often the live queue is re-fetched and the session persisted.
pub(crate) const SYNC_EVERY: Duration = Duration::from_secs(10);

/// Whether this frame is worth drawing.
///
/// Input beats animation beats the idle clock. Smoothness of the recolour comes
/// from its duration, not from the frame rate: every present makes the terminal
/// recompose the viewport, and the inline cover shimmers if that happens 60
/// times a second.
pub(crate) fn should_draw(dirty: bool, animating: bool, since_last: Duration) -> bool {
    if dirty {
        since_last >= MIN_FRAME
    } else if animating {
        since_last >= ANIM_FRAME
    } else {
        since_last >= IDLE_REDRAW
    }
}
