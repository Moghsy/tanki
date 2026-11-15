//! Plain-data model of a note type that the TUI edits as a draft, and the
//! pure operations on it (PLAN 8.6). Nothing here touches a collection.
//!
//! The `ord` of each field and template is the position it had when the
//! note type was loaded (`None` for an item added to the draft). Operations
//! keep it on every surviving item: that is the whole schema-change API of
//! Anki, which diffs the ordinals on save and rewrites notes and cards
//! (R7 §4.2).
use crate::{CoreError, NotetypeKind, Result};
use serde::{Deserialize, Serialize};

/// Anki's stock note type kinds (`StockNotetype.Kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum StockKind {
    Basic,
    BasicAndReversed,
    BasicOptionalReversed,
    BasicTyping,
    Cloze,
    ImageOcclusion,
}

impl StockKind {
    pub const ALL: [StockKind; 6] = [
        Self::Basic,
        Self::BasicAndReversed,
        Self::BasicOptionalReversed,
        Self::BasicTyping,
        Self::Cloze,
        Self::ImageOcclusion,
    ];

    pub(crate) fn to_proto(self) -> anki_proto::notetypes::stock_notetype::Kind {
        use anki_proto::notetypes::stock_notetype::Kind;
        match self {
            Self::Basic => Kind::Basic,
            Self::BasicAndReversed => Kind::BasicAndReversed,
            Self::BasicOptionalReversed => Kind::BasicOptionalReversed,
            Self::BasicTyping => Kind::BasicTyping,
            Self::Cloze => Kind::Cloze,
            Self::ImageOcclusion => Kind::ImageOcclusion,
        }
    }

    /// From `Notetype.Config.original_stock_kind`; `None` when unknown (note
    /// types made before Anki 2.1.62).
    pub(crate) fn from_original(value: i32) -> Option<Self> {
        use anki_proto::notetypes::stock_notetype::OriginalStockKind as O;
        Some(match O::try_from(value).ok()? {
            O::Unknown => return None,
            O::Basic => Self::Basic,
            O::BasicAndReversed => Self::BasicAndReversed,
            O::BasicOptionalReversed => Self::BasicOptionalReversed,
            O::BasicTyping => Self::BasicTyping,
            O::Cloze => Self::Cloze,
            O::ImageOcclusion => Self::ImageOcclusion,
        })
    }
}

/// A stock kind with Anki's localised name for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StockKindInfo {
    pub kind: StockKind,
    pub name: String,
}

/// One row of the note type list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotetypeSummary {
    pub id: i64,
    pub name: String,
    pub kind: NotetypeKind,
    /// Notes of this type (`GetNotetypeNamesAndCounts`).
    pub use_count: u32,
    pub field_count: usize,
    pub template_count: usize,
    /// `None` for note types without a recorded stock origin.
    pub stock_kind: Option<StockKind>,
}

/// A field with every option the fields editor shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldDetail {
    /// Position when loaded; `None` for a field added to the draft.
    pub ord: Option<u32>,
    pub name: String,
    pub sticky: bool,
    pub rtl: bool,
    /// "Use HTML editor by default".
    pub plain_text: bool,
    pub collapsed: bool,
    pub exclude_from_search: bool,
    pub description: String,
    pub font_name: String,
    pub font_size: u32,
    /// Cloze text field, image-occlusion field or flagged
    /// `prevent_deletion`: cannot be deleted or renamed. Computed when
    /// loaded and again, from the stored note type, on save.
    pub required: bool,
}

/// A card type with its editable formats.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateDetail {
    /// Position when loaded; `None` for a template added to the draft.
    pub ord: Option<u32>,
    pub name: String,
    /// Front template.
    pub question: String,
    /// Back template.
    pub answer: String,
    /// Browser appearance question/answer formats; empty means unused.
    pub browser_question: String,
    pub browser_answer: String,
    pub browser_font_name: String,
    pub browser_font_size: u32,
    /// Deck override: new cards of this card type go to this deck.
    pub target_deck_id: Option<i64>,
}

/// A whole note type as a draft. `sort_field` is an index into `fields`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotetypeDetail {
    pub id: i64,
    pub name: String,
    pub kind: NotetypeKind,
    pub stock_kind: Option<StockKind>,
    pub use_count: u32,
    pub css: String,
    pub latex_pre: String,
    pub latex_post: String,
    pub latex_svg: bool,
    pub sort_field: usize,
    pub fields: Vec<FieldDetail>,
    pub templates: Vec<TemplateDetail>,
}

/// An edit applied to a draft (`NotetypeDetail::apply`). Indices refer to the
/// draft as it stands when the edit runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum NotetypeEdit {
    Rename(String),
    SetCss(String),
    SetLatexPre(String),
    SetLatexPost(String),
    SetLatexSvg(bool),
    AddField {
        name: String,
    },
    RenameField {
        index: usize,
        name: String,
    },
    DeleteField {
        index: usize,
    },
    /// Removes the field at `from` and reinserts it at `to`.
    MoveField {
        from: usize,
        to: usize,
    },
    SetSortField {
        index: usize,
    },
    /// Replaces the options of a field (name, `ord` and `required` ignored).
    SetFieldOptions {
        index: usize,
        field: FieldDetail,
    },
    /// Adds a card type that starts as a copy of the last one's formats, as
    /// desktop does.
    AddTemplate {
        name: String,
    },
    RenameTemplate {
        index: usize,
        name: String,
    },
    DeleteTemplate {
        index: usize,
    },
    MoveTemplate {
        from: usize,
        to: usize,
    },
    /// Replaces everything but the name and `ord` of a card type.
    SetTemplate {
        index: usize,
        template: TemplateDetail,
    },
}

fn invalid<T>(message: impl Into<String>) -> Result<T> {
    Err(CoreError::Invalid(message.into()))
}

/// Anki's field name rules as desktop's `_uniqueName` applies them: quotes
/// removed, no leading `#^/`, none of `:{}"` inside.
pub fn check_field_name(raw: &str) -> Result<String> {
    let name = raw.replace('"', "");
    let name = name.trim();
    if name.is_empty() {
        return invalid("The field name is empty");
    }
    if name.starts_with(['#', '^', '/']) {
        return invalid("A field name cannot start with #, ^ or /");
    }
    if name.contains([':', '{', '}']) {
        return invalid("A field name cannot contain :, { or }");
    }
    Ok(name.to_string())
}

fn check_template_name(raw: &str) -> Result<String> {
    let name = raw.replace('"', "");
    let name = name.trim();
    if name.is_empty() {
        return invalid("The card type name is empty");
    }
    Ok(name.to_string())
}

fn index_of<T>(items: &[T], index: usize, what: &str) -> Result<usize> {
    if index < items.len() {
        Ok(index)
    } else {
        invalid(format!("There is no {what} number {}", index + 1))
    }
}

/// Moves `items[from]` to position `to`, returning where the item that was
/// at `track` ended up.
fn reposition<T>(items: &mut Vec<T>, from: usize, to: usize, track: usize) -> usize {
    let mut tagged: Vec<(usize, T)> = std::mem::take(items).into_iter().enumerate().collect();
    let moved = tagged.remove(from);
    tagged.insert(to, moved);
    let position = tagged
        .iter()
        .position(|(original, _)| *original == track)
        .unwrap_or(0);
    *items = tagged.into_iter().map(|(_, item)| item).collect();
    position
}

impl NotetypeDetail {
    /// Applies one edit. Invalid edits leave the draft unchanged.
    pub fn apply(&mut self, edit: NotetypeEdit) -> Result<()> {
        match edit {
            NotetypeEdit::Rename(name) => {
                let name = name.replace('"', "");
                if name.trim().is_empty() {
                    return invalid("The note type name is empty");
                }
                self.name = name.trim().to_string();
            }
            NotetypeEdit::SetCss(css) => self.css = css,
            NotetypeEdit::SetLatexPre(text) => self.latex_pre = text,
            NotetypeEdit::SetLatexPost(text) => self.latex_post = text,
            NotetypeEdit::SetLatexSvg(on) => self.latex_svg = on,
            NotetypeEdit::AddField { name } => {
                let name = self.unique_field_name(&check_field_name(&name)?, None)?;
                self.fields.push(FieldDetail {
                    ord: None,
                    name,
                    sticky: false,
                    rtl: false,
                    plain_text: false,
                    collapsed: false,
                    exclude_from_search: false,
                    description: String::new(),
                    font_name: "Arial".into(),
                    font_size: 20,
                    required: false,
                });
            }
            NotetypeEdit::RenameField { index, name } => {
                index_of(&self.fields, index, "field")?;
                let name = check_field_name(&name)?;
                if name == self.fields[index].name {
                    return Ok(());
                }
                if self.fields[index].required {
                    return invalid(format!(
                        "The field {} is required by this note type and cannot be renamed",
                        self.fields[index].name
                    ));
                }
                self.fields[index].name = self.unique_field_name(&name, Some(index))?;
            }
            NotetypeEdit::DeleteField { index } => {
                index_of(&self.fields, index, "field")?;
                if self.fields.len() < 2 {
                    return invalid("Notes require at least one field");
                }
                if self.fields[index].required {
                    return invalid(format!(
                        "The field {} is required by this note type and cannot be deleted",
                        self.fields[index].name
                    ));
                }
                self.fields.remove(index);
                // As the desktop's `remove_field`: the sort field falls back
                // to the first one when it is deleted.
                self.sort_field = match self.sort_field.cmp(&index) {
                    std::cmp::Ordering::Less => self.sort_field,
                    std::cmp::Ordering::Equal => 0,
                    std::cmp::Ordering::Greater => self.sort_field - 1,
                };
            }
            NotetypeEdit::MoveField { from, to } => {
                index_of(&self.fields, from, "field")?;
                index_of(&self.fields, to, "field")?;
                self.sort_field = reposition(&mut self.fields, from, to, self.sort_field);
            }
            NotetypeEdit::SetSortField { index } => {
                index_of(&self.fields, index, "field")?;
                self.sort_field = index;
            }
            NotetypeEdit::SetFieldOptions { index, field } => {
                index_of(&self.fields, index, "field")?;
                let target = &mut self.fields[index];
                target.sticky = field.sticky;
                target.rtl = field.rtl;
                target.plain_text = field.plain_text;
                target.collapsed = field.collapsed;
                target.exclude_from_search = field.exclude_from_search;
                target.description = field.description;
                target.font_name = field.font_name;
                target.font_size = field.font_size;
            }
            NotetypeEdit::AddTemplate { name } => {
                if self.kind == NotetypeKind::Cloze {
                    return invalid("A cloze note type has exactly one card type");
                }
                let name = check_template_name(&name)?;
                let mut template = self.templates.last().cloned().unwrap_or(TemplateDetail {
                    ord: None,
                    name: String::new(),
                    question: String::new(),
                    answer: String::new(),
                    browser_question: String::new(),
                    browser_answer: String::new(),
                    browser_font_name: String::new(),
                    browser_font_size: 0,
                    target_deck_id: None,
                });
                template.ord = None;
                template.name = name;
                self.templates.push(template);
            }
            NotetypeEdit::RenameTemplate { index, name } => {
                index_of(&self.templates, index, "card type")?;
                self.templates[index].name = check_template_name(&name)?;
            }
            NotetypeEdit::DeleteTemplate { index } => {
                index_of(&self.templates, index, "card type")?;
                if self.kind == NotetypeKind::Cloze || self.templates.len() < 2 {
                    return invalid("A note type needs at least one card type");
                }
                self.templates.remove(index);
            }
            NotetypeEdit::MoveTemplate { from, to } => {
                index_of(&self.templates, from, "card type")?;
                index_of(&self.templates, to, "card type")?;
                reposition(&mut self.templates, from, to, 0);
            }
            NotetypeEdit::SetTemplate { index, template } => {
                index_of(&self.templates, index, "card type")?;
                let target = &mut self.templates[index];
                target.question = template.question;
                target.answer = template.answer;
                target.browser_question = template.browser_question;
                target.browser_answer = template.browser_answer;
                target.browser_font_name = template.browser_font_name;
                target.browser_font_size = template.browser_font_size;
                target.target_deck_id = template.target_deck_id;
            }
        }
        Ok(())
    }

    /// `name`, or an error when another field (case-insensitively, as desktop
    /// compares) already has it.
    fn unique_field_name(&self, name: &str, skip: Option<usize>) -> Result<String> {
        let folded = name.to_lowercase();
        let clash = self
            .fields
            .iter()
            .enumerate()
            .any(|(i, f)| Some(i) != skip && f.name.to_lowercase() == folded);
        if clash {
            invalid("That field name is already used")
        } else {
            Ok(name.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(ord: u32, name: &str) -> FieldDetail {
        FieldDetail {
            ord: Some(ord),
            name: name.into(),
            sticky: false,
            rtl: false,
            plain_text: false,
            collapsed: false,
            exclude_from_search: false,
            description: String::new(),
            font_name: "Arial".into(),
            font_size: 20,
            required: false,
        }
    }

    fn template(ord: u32, name: &str) -> TemplateDetail {
        TemplateDetail {
            ord: Some(ord),
            name: name.into(),
            question: format!("{{{{{name}}}}}"),
            answer: String::new(),
            browser_question: String::new(),
            browser_answer: String::new(),
            browser_font_name: String::new(),
            browser_font_size: 0,
            target_deck_id: None,
        }
    }

    fn draft() -> NotetypeDetail {
        NotetypeDetail {
            id: 1,
            name: "Test".into(),
            kind: NotetypeKind::Normal,
            stock_kind: None,
            use_count: 0,
            css: String::new(),
            latex_pre: String::new(),
            latex_post: String::new(),
            latex_svg: false,
            sort_field: 0,
            fields: vec![field(0, "A"), field(1, "B"), field(2, "C")],
            templates: vec![template(0, "One"), template(1, "Two")],
        }
    }

    fn ords(d: &NotetypeDetail) -> Vec<Option<u32>> {
        d.fields.iter().map(|f| f.ord).collect()
    }

    #[test]
    fn moves_keep_ordinals_and_the_sort_field_follows() {
        let mut d = draft();
        d.sort_field = 2;
        d.apply(NotetypeEdit::MoveField { from: 2, to: 0 }).unwrap();
        assert_eq!(ords(&d), [Some(2), Some(0), Some(1)]);
        assert_eq!(d.sort_field, 0);
        d.apply(NotetypeEdit::MoveField { from: 1, to: 2 }).unwrap();
        assert_eq!(ords(&d), [Some(2), Some(1), Some(0)]);
        assert_eq!(d.sort_field, 0);
        d.apply(NotetypeEdit::MoveField { from: 0, to: 2 }).unwrap();
        assert_eq!(d.sort_field, 2);
        assert!(d.apply(NotetypeEdit::MoveField { from: 0, to: 3 }).is_err());
    }

    #[test]
    fn add_delete_and_sort_field_rules() {
        let mut d = draft();
        d.apply(NotetypeEdit::AddField { name: "D".into() })
            .unwrap();
        assert_eq!(d.fields[3].ord, None);
        assert!(d
            .apply(NotetypeEdit::AddField { name: "d".into() })
            .is_err());
        d.apply(NotetypeEdit::SetSortField { index: 1 }).unwrap();
        d.apply(NotetypeEdit::DeleteField { index: 0 }).unwrap();
        assert_eq!(d.sort_field, 0);
        d.apply(NotetypeEdit::DeleteField { index: 0 }).unwrap();
        assert_eq!(d.sort_field, 0);
        assert_eq!(ords(&d), [Some(2), None]);
        d.apply(NotetypeEdit::DeleteField { index: 1 }).unwrap();
        assert!(d.apply(NotetypeEdit::DeleteField { index: 0 }).is_err());
    }

    #[test]
    fn field_names_follow_desktop_rules() {
        assert_eq!(check_field_name(" \"Front\" ").unwrap(), "Front");
        for bad in ["", "#x", "^x", "/x", "a:b", "a{b", "a}b"] {
            assert!(check_field_name(bad).is_err(), "{bad}");
        }
        let mut d = draft();
        assert!(d
            .apply(NotetypeEdit::RenameField {
                index: 0,
                name: "b".into()
            })
            .is_err());
        // Changing only the case of its own name is fine.
        d.apply(NotetypeEdit::RenameField {
            index: 0,
            name: "a2".into(),
        })
        .unwrap();
        d.fields[1].required = true;
        assert!(d
            .apply(NotetypeEdit::RenameField {
                index: 1,
                name: "Z".into()
            })
            .is_err());
        assert!(d.apply(NotetypeEdit::DeleteField { index: 1 }).is_err());
    }

    #[test]
    fn templates_copy_the_last_and_cloze_has_one() {
        let mut d = draft();
        d.apply(NotetypeEdit::AddTemplate {
            name: "Three".into(),
        })
        .unwrap();
        assert_eq!(d.templates[2].ord, None);
        assert_eq!(d.templates[2].question, "{{Two}}");
        d.apply(NotetypeEdit::MoveTemplate { from: 2, to: 0 })
            .unwrap();
        assert_eq!(d.templates[0].name, "Three");
        assert_eq!(d.templates[1].ord, Some(0));
        d.apply(NotetypeEdit::DeleteTemplate { index: 0 }).unwrap();
        d.apply(NotetypeEdit::DeleteTemplate { index: 0 }).unwrap();
        assert!(d.apply(NotetypeEdit::DeleteTemplate { index: 0 }).is_err());
        d.kind = NotetypeKind::Cloze;
        assert!(d
            .apply(NotetypeEdit::AddTemplate { name: "x".into() })
            .is_err());
    }
}
