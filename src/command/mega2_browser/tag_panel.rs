//! TUI tag panel state (plan-20260912 MB-11).
//!
//! The panel is a modal view over the same `browser` command: `t` opens it
//! (fetching page 1 only), `n`/`p` request the explicit next/previous page,
//! `+` creates a tag (name, then an optional message; an empty message is a
//! lightweight tag), `d` deletes after an extra confirmation, and `q`/`Esc`
//! close the panel without leaving the browser. All server strings (tag name,
//! tagger, message) are sanitized before rendering.

use super::{Key, push_bounded, sanitize};
use crate::internal::protocol::mega2_tag::{TagInfo, validate_tag_name};

/// Fixed page size for the panel (one GET per explicit page change).
pub const TAG_PAGE_SIZE: u64 = 20;
/// Bound for the optional annotated-tag message.
pub const MAX_TAG_MESSAGE_BYTES: usize = 1024;

/// The panel's own modal editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TagEditor {
    /// `+`: collect the tag name first.
    CreateName(String),
    /// After a valid name: collect an optional message (empty = lightweight).
    CreateMessage { name: String, message: String },
    /// `d`: extra confirmation line before deleting.
    ConfirmDelete(String),
}

/// What the event loop must do after a panel key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TagPanelAction {
    Continue,
    /// Close the panel (back to the directory listing).
    Close,
    /// One explicit page fetch; no prefetch.
    Fetch {
        page: u64,
    },
    /// One confirmed create.
    Create {
        name: String,
        /// `None` is a lightweight tag (empty message).
        message: Option<String>,
    },
    /// One confirmed delete.
    Delete {
        name: String,
    },
}

/// Tag panel state: bounded, page-at-a-time, modal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagPanel {
    pub page: u64,
    pub per_page: u64,
    pub total: u64,
    pub items: Vec<TagInfo>,
    pub selection: usize,
    pub editor: Option<TagEditor>,
    pub status: Option<String>,
}

impl Default for TagPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl TagPanel {
    /// A fresh panel: page 1, empty, no editor.
    pub fn new() -> Self {
        Self {
            page: 1,
            per_page: TAG_PAGE_SIZE,
            total: 0,
            items: Vec::new(),
            selection: 0,
            editor: None,
            status: None,
        }
    }

    /// Applies a fetched page.
    pub fn apply_page(&mut self, page: u64, total: u64, items: Vec<TagInfo>) {
        self.page = page;
        self.total = total;
        self.items = items;
        if self.selection >= self.items.len() {
            self.selection = 0;
        }
        self.status = None;
    }

    /// Whether an explicit next page exists.
    pub fn has_next(&self) -> bool {
        self.page.saturating_mul(self.per_page) < self.total
    }

    /// Whether an explicit previous page exists.
    pub fn has_prev(&self) -> bool {
        self.page > 1
    }

    /// The selected tag name, if any.
    pub fn selected_name(&self) -> Option<String> {
        self.items.get(self.selection).map(|tag| tag.name.clone())
    }

    fn handle_editor_key(&mut self, key: Key) -> TagPanelAction {
        let Some(editor) = self.editor.take() else {
            return TagPanelAction::Continue;
        };
        match editor {
            TagEditor::CreateName(mut input) => match key {
                Key::Enter => match validate_tag_name(&input) {
                    Ok(()) => {
                        self.editor = Some(TagEditor::CreateMessage {
                            name: input,
                            message: String::new(),
                        });
                        TagPanelAction::Continue
                    }
                    Err(err) => {
                        self.status = Some(err.message().to_string());
                        self.editor = Some(TagEditor::CreateName(input));
                        TagPanelAction::Continue
                    }
                },
                Key::Backspace => {
                    input.pop();
                    self.editor = Some(TagEditor::CreateName(input));
                    TagPanelAction::Continue
                }
                Key::Cancel | Key::Quit => {
                    self.status = Some("tag create cancelled".to_string());
                    TagPanelAction::Continue
                }
                Key::Other(ch) => {
                    push_bounded(&mut input, ch);
                    self.editor = Some(TagEditor::CreateName(input));
                    TagPanelAction::Continue
                }
                _ => {
                    self.editor = Some(TagEditor::CreateName(input));
                    TagPanelAction::Continue
                }
            },
            TagEditor::CreateMessage { name, mut message } => match key {
                Key::Enter => TagPanelAction::Create {
                    name,
                    // Empty message means a lightweight tag.
                    message: if message.is_empty() {
                        None
                    } else {
                        Some(message)
                    },
                },
                Key::Backspace => {
                    message.pop();
                    self.editor = Some(TagEditor::CreateMessage { name, message });
                    TagPanelAction::Continue
                }
                Key::Cancel | Key::Quit => {
                    self.status = Some("tag create cancelled".to_string());
                    TagPanelAction::Continue
                }
                Key::Other(ch) => {
                    if !ch.is_control() && message.len() + ch.len_utf8() <= MAX_TAG_MESSAGE_BYTES {
                        message.push(ch);
                    }
                    self.editor = Some(TagEditor::CreateMessage { name, message });
                    TagPanelAction::Continue
                }
                _ => {
                    self.editor = Some(TagEditor::CreateMessage { name, message });
                    TagPanelAction::Continue
                }
            },
            TagEditor::ConfirmDelete(name) => match key {
                Key::Enter | Key::Other('y') | Key::Other('Y') => TagPanelAction::Delete { name },
                Key::Cancel | Key::Quit | Key::Other('n') | Key::Other('N') => {
                    self.status = Some("tag delete cancelled".to_string());
                    TagPanelAction::Continue
                }
                _ => {
                    self.editor = Some(TagEditor::ConfirmDelete(name));
                    TagPanelAction::Continue
                }
            },
        }
    }

    /// Handles one key while the panel is open.
    pub fn handle_key(&mut self, key: Key) -> TagPanelAction {
        if self.editor.is_some() {
            return self.handle_editor_key(key);
        }
        match key {
            Key::Up => {
                if self.selection > 0 {
                    self.selection -= 1;
                }
                TagPanelAction::Continue
            }
            Key::Down => {
                if !self.items.is_empty() && self.selection + 1 < self.items.len() {
                    self.selection += 1;
                }
                TagPanelAction::Continue
            }
            Key::Cancel | Key::Quit => TagPanelAction::Close,
            // Panel paging is explicit: one fetch per key, never prefetched.
            Key::Other('n') => {
                if self.has_next() {
                    TagPanelAction::Fetch {
                        page: self.page + 1,
                    }
                } else {
                    self.status = Some("last tag page".to_string());
                    TagPanelAction::Continue
                }
            }
            Key::Other('p') => {
                if self.has_prev() {
                    TagPanelAction::Fetch {
                        page: self.page - 1,
                    }
                } else {
                    self.status = Some("first tag page".to_string());
                    TagPanelAction::Continue
                }
            }
            Key::Create => {
                self.editor = Some(TagEditor::CreateName(String::new()));
                self.status = None;
                TagPanelAction::Continue
            }
            Key::Delete => {
                match self.selected_name() {
                    Some(name) => {
                        self.editor = Some(TagEditor::ConfirmDelete(name));
                        self.status = None;
                    }
                    None => {
                        self.status = Some("no tag selected".to_string());
                    }
                }
                TagPanelAction::Continue
            }
            // `t` toggles the panel closed as well.
            Key::Tags => TagPanelAction::Close,
            _ => TagPanelAction::Continue,
        }
    }

    /// Panel body lines (already sanitized).
    pub fn render_lines(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "mega2 tags — page {} ({} tags) — path /\r\n",
            self.page, self.total
        ));
        out.push_str("──────────────────────────────────────────────\r\n");
        if self.items.is_empty() {
            out.push_str("(no tags on this page)\r\n");
        }
        for (idx, tag) in self.items.iter().enumerate() {
            let marker = if idx == self.selection { ">" } else { " " };
            out.push_str(&format!(
                "{marker} {}  {}  {}\r\n",
                sanitize(&tag.name),
                sanitize(&tag.object_type),
                sanitize(&tag.tagger)
            ));
            if !tag.message.is_empty() {
                out.push_str(&format!("    {}\r\n", sanitize(&tag.message)));
            }
        }
        out.push_str("──────────────────────────────────────────────\r\n");
        match self.editor.as_ref() {
            Some(TagEditor::CreateName(input)) => {
                out.push_str(&format!("new tag name: {}\r\n", sanitize(input)));
                out.push_str("Enter continue · Esc cancel\r\n");
            }
            Some(TagEditor::CreateMessage { name, message }) => {
                out.push_str(&format!(
                    "tag '{}' message (empty = lightweight): {}\r\n",
                    sanitize(name),
                    sanitize(message)
                ));
                out.push_str("Enter create · Esc cancel\r\n");
            }
            Some(TagEditor::ConfirmDelete(name)) => {
                out.push_str(&format!(
                    "delete tag '{}'? Enter/y confirm · Esc cancel\r\n",
                    sanitize(name)
                ));
            }
            None => {}
        }
        let status = self.status.as_deref().unwrap_or(
            "Up/Down select · n next · p prev · + create · d delete · t/Esc close panel",
        );
        out.push_str(&sanitize(status));
        out.push_str("\r\n");
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tag(name: &str, tagger: &str, message: &str) -> TagInfo {
        TagInfo {
            name: name.to_string(),
            tag_id: "t".to_string(),
            object_id: "o".to_string(),
            object_type: "commit".to_string(),
            tagger: tagger.to_string(),
            message: message.to_string(),
            created_at: "2026-09-21T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn paging_is_explicit_and_bounded() {
        let mut panel = TagPanel::new();
        assert!(!panel.has_next(), "empty panel has no next page");
        assert!(!panel.has_prev());

        panel.apply_page(1, 45, vec![tag("v1", "a", "")]);
        assert!(panel.has_next(), "45 tags = more than one page");

        panel.apply_page(3, 45, vec![]);
        assert!(!panel.has_next(), "last page");
        assert!(panel.has_prev());
    }

    #[test]
    fn create_flow_collects_name_then_optional_message() {
        let mut panel = TagPanel::new();
        assert_eq!(panel.handle_key(Key::Create), TagPanelAction::Continue);
        for ch in "v1.0.0".chars() {
            panel.handle_key(Key::Other(ch));
        }
        assert_eq!(panel.handle_key(Key::Enter), TagPanelAction::Continue);
        assert!(matches!(
            panel.editor,
            Some(TagEditor::CreateMessage { .. })
        ));

        // Empty message submits a lightweight tag.
        let action = panel.handle_key(Key::Enter);
        assert_eq!(
            action,
            TagPanelAction::Create {
                name: "v1.0.0".to_string(),
                message: None
            }
        );

        // A message submits an annotated tag.
        panel.handle_key(Key::Create);
        for ch in "v2".chars() {
            panel.handle_key(Key::Other(ch));
        }
        panel.handle_key(Key::Enter);
        for ch in "release".chars() {
            panel.handle_key(Key::Other(ch));
        }
        assert_eq!(
            panel.handle_key(Key::Enter),
            TagPanelAction::Create {
                name: "v2".to_string(),
                message: Some("release".to_string())
            }
        );
    }

    #[test]
    fn hostile_names_never_reach_the_action() {
        let mut panel = TagPanel::new();
        for bad in ["..", "a..b", "a//b", "release.lock", "has space"] {
            panel.editor = Some(TagEditor::CreateName(bad.to_string()));
            let action = panel.handle_key(Key::Enter);
            assert_eq!(action, TagPanelAction::Continue, "{bad:?}");
            assert!(matches!(panel.editor, Some(TagEditor::CreateName(_))));
            assert!(panel.status.is_some());
        }
    }

    #[test]
    fn delete_requires_confirmation_and_esc_closes_the_panel() {
        let mut panel = TagPanel::new();
        panel.apply_page(1, 1, vec![tag("v1", "a", "")]);
        assert_eq!(panel.handle_key(Key::Delete), TagPanelAction::Continue);
        assert!(matches!(
            panel.editor,
            Some(TagEditor::ConfirmDelete(ref n)) if n == "v1"
        ));

        // Esc cancels the deletion, not the panel.
        assert_eq!(panel.handle_key(Key::Cancel), TagPanelAction::Continue);
        assert!(panel.editor.is_none());
        assert_eq!(panel.status.as_deref(), Some("tag delete cancelled"));

        // Esc on the panel itself closes it.
        assert_eq!(panel.handle_key(Key::Cancel), TagPanelAction::Close);
        assert_eq!(panel.handle_key(Key::Tags), TagPanelAction::Close);
    }

    #[test]
    fn renderer_escapes_hostile_tag_fields() {
        let mut panel = TagPanel::new();
        panel.apply_page(
            1,
            1,
            vec![tag("v1", "tagger\u{1b}[2J", "line1\u{7}\nline2")],
        );
        let rendered = panel.render_lines();
        assert!(!rendered.contains('\u{1b}'), "control chars escaped");
        assert!(!rendered.contains('\u{7}'), "bell escaped");
        assert!(rendered.contains("line1??line2"), "{rendered}");
    }
}
