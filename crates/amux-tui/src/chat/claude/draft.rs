//! The one review a chat can be drafting.
//!
//! A review is not sent when it is written: it lives in the draft as an
//! atomic token beside whatever else was typed, and only the send turns it
//! into an element. Everything about the review itself — the frozen diff,
//! the cursor, the comments — belongs to the page; this type holds the two
//! facts the chat needs about it: whether the page is on screen, and which
//! token in the draft stands for it.

use serde::{Deserialize, Serialize};

use crate::composer::{Composer, TokenAttachment, token_label};
use crate::review::ReviewView;

/// The name a review token carries; the label counts comments instead.
const REVIEW_ORDINAL: usize = 1;

/// The branch `b` offers as the other base to review against.
///
/// Nothing the daemon reports names a repository's trunk, and the review
/// page cannot invent one, so the chat states the near-universal default.
/// A repository that calls its trunk something else needs `b` to be told;
/// that is a fact to plumb through, not a guess to make here.
pub const BRANCH_BASE: &str = "main";

/// The review a chat is drafting: the page and its place in the draft.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReviewDraft {
    /// The token standing for the review in the draft. Absent until the
    /// first comment is saved: a review nobody wrote on is not yet
    /// something to send, so it takes no room in the text.
    pub slot: Option<char>,
    /// The page is on screen, over the whole chat frame.
    pub open: bool,
    pub view: ReviewView,
}

impl ReviewDraft {
    /// A freshly frozen diff, with its page open.
    pub fn opened(view: ReviewView) -> Self {
        Self {
            slot: None,
            open: true,
            view,
        }
    }

    /// The label the token wears: what a person needs in order to know
    /// what pressing Enter on it would reopen.
    pub fn label(&self) -> String {
        let count = self.view.review().comment_count();
        let unit = if count == 1 { "comment" } else { "comments" };
        token_label(
            &TokenAttachment::Review,
            REVIEW_ORDINAL,
            "review",
            Some(&format!("{count} {unit}")),
        )
    }

    /// Put the review in the draft, or bring its label up to date with the
    /// comments behind it. The token appears at the cursor the moment the
    /// first comment is saved, so a review with nothing said about it never
    /// clutters a draft the person may not send.
    pub fn sync_token(&mut self, composer: &mut Composer) {
        if self.view.review().comment_count() == 0 {
            return;
        }
        let label = self.label();
        match self.slot {
            Some(slot) => composer.set_token_label(slot, label),
            None => self.slot = Some(composer.insert_token(label, TokenAttachment::Review)),
        }
    }
}
