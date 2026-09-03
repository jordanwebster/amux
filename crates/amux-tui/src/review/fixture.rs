//! One frozen three-file review, shared by the page's unit tests, its
//! goldens, and the screenshot states, so every capture of the review page
//! shows the same diff.

use amux_ui::attachments::ArtifactId;
use amux_ui::review::{Review, RowRef, anchor, parse_patch};
use amux_ui::{ArtifactKind, ArtifactRef, BaseIdentity, DiffBase, DiffFile, DiffResponse};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::ReviewView;

const PATCH: &str = "diff --git a/src/lib.rs b/src/lib.rs
index 1111111..2222222 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,3 +1,3 @@
 pub mod artifacts;
-pub mod legacy_store;
+pub mod attachments;
 pub mod review;
@@ -10,3 +10,5 @@ impl Session {
     pub fn open(&self) -> Result<Handle> {
-        self.connect(Timeout::default())
+        // A review holds its diff open for the whole page, so the handle outlives the request that produced it and nothing refetches.
+        // The base is frozen with it.
+        self.connect(Timeout::long())
     }
diff --git a/notes/old-plan.md b/notes/old-plan.md
deleted file mode 100644
index 3333333..0000000
--- a/notes/old-plan.md
+++ /dev/null
@@ -1,2 +0,0 @@
-Store every attachment in the session log.
-Re-read it on restart.
diff --git a/src/attachments.rs b/src/attachments.rs
new file mode 100644
index 0000000..4444444
--- /dev/null
+++ b/src/attachments.rs
@@ -0,0 +1,3 @@
+//! Message attachments, addressed by content.
+
+pub struct Attachment;";

fn files() -> Vec<DiffFile> {
    vec![
        DiffFile {
            path: "src/lib.rs".into(),
            added: 4,
            removed: 2,
        },
        DiffFile {
            path: "notes/old-plan.md".into(),
            added: 0,
            removed: 2,
        },
        DiffFile {
            path: "src/attachments.rs".into(),
            added: 3,
            removed: 0,
        },
    ]
}

/// The artifact the daemon stored the frozen patch as.
const DIFF_ID: &str = "sha256:8c1f0d2e5a7b4c9d0e1f2a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f";

fn diff_id() -> ArtifactId {
    DIFF_ID.parse::<ArtifactId>().expect("fixture artifact id")
}

/// The same diff as the daemon hands back, so the chat that hosts the page
/// opens over exactly what the page's own captures show.
pub fn sample_diff_response(base: DiffBase) -> DiffResponse {
    DiffResponse {
        artifact: ArtifactRef {
            id: diff_id(),
            kind: ArtifactKind::Diff,
            name: "review".into(),
            mime: "text/x-diff".into(),
            size: PATCH.len() as u64,
        },
        patch: PATCH.to_string(),
        identity: identity(base),
        files: files(),
    }
}

fn identity(base: DiffBase) -> BaseIdentity {
    BaseIdentity {
        base,
        head: "4f2a9c1".into(),
        merge_base: None,
        blobs: vec![
            ("src/lib.rs".into(), "2222222".into()),
            ("src/attachments.rs".into(), "4444444".into()),
        ],
    }
}

/// The review as it opens against the working tree.
pub fn sample_review() -> ReviewView {
    sample_review_against(DiffBase::WorkingTree)
}

/// The same diff attributed to another base, for the branch-base capture.
pub fn sample_review_against(base: DiffBase) -> ReviewView {
    let document = parse_patch(PATCH, identity(base), &files()).expect("fixture patch parses");
    let core = Review::new(document, diff_id());
    ReviewView::new(core, "main")
}

/// The same review after two comments were saved, for the captures that
/// have to show comment counts.
pub fn sample_review_with_comments() -> ReviewView {
    let mut view = sample_review();
    for (file, row, text) in [
        (0usize, 3usize, "Say why the store had to go."),
        (2, 1, "Name the crate this belongs to."),
        (2, 3, "Give it a doc comment."),
    ] {
        let at = RowRef { file, row };
        let anchor = anchor(view.review().document(), at, at).expect("fixture row anchors");
        view.review_mut().add(anchor, text.to_string());
    }
    view
}

/// The page mid-selection: a removed row and the added row under it.
pub fn sample_review_selecting() -> ReviewView {
    let mut view = sample_review();
    view.set_viewport(120, 40);
    for code in ['j', 'j', 'v', 'j'] {
        view.handle_key(&KeyEvent::new(KeyCode::Char(code), KeyModifiers::NONE));
    }
    view
}

/// The same selection with the comment box open over it.
pub fn sample_review_commenting() -> ReviewView {
    let mut view = sample_review_selecting();
    view.handle_key(&KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE));
    for character in "the old name is public; keep a re-export for one release".chars() {
        view.handle_key(&KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
    }
    view
}
