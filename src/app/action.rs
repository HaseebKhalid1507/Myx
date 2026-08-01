//! The context actions overlay: what an entry does and what's on the menu.

/// What an action-menu entry does when activated.
#[derive(Clone)]
pub(crate) enum ActionKind {
    ToggleLike {
        id: String,
        saved: bool,
    },
    Queue {
        uri: String,
    },
    AddToPlaylistMenu {
        track_uri: String,
    },
    AddToPlaylist {
        playlist_id: String,
        track_uri: String,
    },
    ToggleFollowArtist {
        id: String,
        following: bool,
    },
    ToggleSaveAlbum {
        id: String,
        saved: bool,
    },
    FollowPlaylist {
        id: String,
    },
    Play {
        uri: String,
        /// Carried so the play path can set `source_name` — without it the
        /// Queue view's PLAYING FROM header and the persisted resume source
        /// go stale.
        name: String,
    },
    Open {
        uri: String,
        name: String,
    },
    CopyLink {
        uri: String,
    },
}

pub(crate) struct ActionItem {
    pub(crate) label: String,
    pub(crate) kind: ActionKind,
}

pub(crate) struct ActionMenu {
    pub(crate) title: String,
    pub(crate) items: Vec<ActionItem>,
    pub(crate) selected: usize,
}
