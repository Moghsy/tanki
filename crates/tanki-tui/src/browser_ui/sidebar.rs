//! The sidebar tree, built client-side from `SidebarData` plus the fixed
//! sections, in desktop's order (`SidebarStage` in
//! `qt/aqt/browser/sidebar/tree.py`): Saved Searches, Today, Flags, Card
//! State, Decks, Note Types, Tags. Each item's search is the text desktop's
//! `SearchNode` writes (R6 §3.1).
use crate::completion::search_term;
use std::collections::HashSet;
use tanki_core::{SidebarData, SidebarDeck, TagNode};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Section {
    Saved,
    Today,
    Flags,
    CardState,
    Decks,
    Notetypes,
    Tags,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemKind {
    Section(Section),
    Saved(String),
    Deck(i64),
    Tag(String),
    Notetype(i64),
    Other,
}

/// One visible row of the tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub depth: usize,
    pub label: String,
    pub search: Option<String>,
    pub kind: ItemKind,
    pub expandable: bool,
    pub collapsed: bool,
}

/// Collapse state the collection does not keep for tanki (sections) or at
/// all (note types).
#[derive(Debug, Clone, Default)]
pub struct LocalState {
    pub collapsed_sections: HashSet<Section>,
    pub expanded_notetypes: HashSet<i64>,
}

/// `field:_*` (a non-empty field) with the name escaped like a value.
fn field_term(name: &str) -> String {
    let mut escaped = String::new();
    for c in name.chars() {
        if matches!(c, '\\' | '"' | '*' | '_' | ':') {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    if name.contains(|c: char| c.is_whitespace() || c == '(' || c == ')') {
        format!("\"{escaped}:_*\"")
    } else {
        format!("{escaped}:_*")
    }
}

struct Builder<'a> {
    rows: Vec<Row>,
    local: &'a LocalState,
}

impl Builder<'_> {
    fn push(
        &mut self,
        depth: usize,
        label: impl Into<String>,
        search: Option<String>,
        kind: ItemKind,
        expandable: bool,
        collapsed: bool,
    ) {
        self.rows.push(Row {
            depth,
            label: label.into(),
            search,
            kind,
            expandable,
            collapsed: expandable && collapsed,
        });
    }

    /// Pushes a section header; true when its children are shown.
    fn section(&mut self, section: Section, label: &str, search: Option<&str>) -> bool {
        let collapsed = self.local.collapsed_sections.contains(&section);
        self.push(
            0,
            label,
            search.map(str::to_owned),
            ItemKind::Section(section),
            true,
            collapsed,
        );
        !collapsed
    }

    fn leaf(&mut self, depth: usize, label: impl Into<String>, search: impl Into<String>) {
        self.push(
            depth,
            label,
            Some(search.into()),
            ItemKind::Other,
            false,
            false,
        );
    }

    fn deck(&mut self, deck: &SidebarDeck, depth: usize) {
        self.push(
            depth,
            if deck.filtered {
                format!("{} (filtered)", deck.name)
            } else {
                deck.name.clone()
            },
            Some(search_term("deck", &deck.full_name)),
            ItemKind::Deck(deck.id),
            !deck.children.is_empty(),
            deck.collapsed,
        );
        if !deck.collapsed {
            for child in &deck.children {
                self.deck(child, depth + 1);
            }
        }
    }

    fn tag(&mut self, tag: &TagNode, depth: usize) {
        self.push(
            depth,
            tag.name.clone(),
            Some(search_term("tag", &tag.full_name)),
            ItemKind::Tag(tag.full_name.clone()),
            !tag.children.is_empty(),
            tag.collapsed,
        );
        if !tag.collapsed {
            for child in &tag.children {
                self.tag(child, depth + 1);
            }
        }
    }
}

/// The visible rows: children of collapsed items are skipped.
pub fn rows(data: &SidebarData, local: &LocalState) -> Vec<Row> {
    let mut b = Builder {
        rows: Vec::new(),
        local,
    };
    if b.section(Section::Saved, "Saved Searches", None) {
        for (name, search) in &data.saved_searches {
            b.push(
                1,
                name.clone(),
                Some(search.clone()),
                ItemKind::Saved(name.clone()),
                false,
                false,
            );
        }
    }
    if b.section(Section::Today, "Today", None) {
        for (label, search) in [
            ("Due Today", "prop:due=0"),
            ("Added Today", "added:1"),
            ("Edited Today", "edited:1"),
            ("Studied Today", "rated:1"),
            ("First Review", "introduced:1"),
            ("Rescheduled", "resched:1"),
            ("Again Today", "rated:1:1"),
            ("Overdue", "(is:due -prop:due=0)"),
        ] {
            b.leaf(1, label, search);
        }
    }
    if b.section(Section::Flags, "Flags", Some("-flag:0")) {
        b.leaf(1, "No Flag", "flag:0");
        for (i, name) in data.flag_names.iter().enumerate() {
            b.leaf(1, name.clone(), format!("flag:{}", i + 1));
        }
    }
    if b.section(Section::CardState, "Card State", None) {
        for (label, search) in [
            ("New", "is:new"),
            ("Learning", "is:learn"),
            ("Review", "is:review"),
            ("Suspended", "is:suspended"),
            ("Buried", "is:buried"),
        ] {
            b.leaf(1, label, search);
        }
    }
    if b.section(Section::Decks, "Decks", Some("deck:_*")) {
        b.leaf(1, "Current Deck", "deck:current");
        for deck in &data.decks {
            b.deck(deck, 1);
        }
    }
    if b.section(Section::Notetypes, "Note Types", Some("note:_*")) {
        for nt in &data.notetypes {
            let expanded = local.expanded_notetypes.contains(&nt.id);
            let note = search_term("note", &nt.name);
            b.push(
                1,
                nt.name.clone(),
                Some(note.clone()),
                ItemKind::Notetype(nt.id),
                true,
                !expanded,
            );
            if expanded {
                for t in &nt.templates {
                    b.leaf(2, t.name.clone(), format!("({note} card:{})", t.ord + 1));
                }
                for f in &nt.fields {
                    b.leaf(2, f.name.clone(), format!("({note} {})", field_term(&f.name)));
                }
            }
        }
    }
    if b.section(Section::Tags, "Tags", Some("tag:_*")) {
        b.leaf(1, "Untagged", "-tag:_*");
        for tag in &data.tags.children {
            b.tag(tag, 1);
        }
    }
    b.rows
}

/// Sets a deck's collapse state in the cached data (after the command).
pub fn set_deck_collapsed(decks: &mut [SidebarDeck], id: i64, collapsed: bool) -> bool {
    for deck in decks {
        if deck.id == id {
            deck.collapsed = collapsed;
            return true;
        }
        if set_deck_collapsed(&mut deck.children, id, collapsed) {
            return true;
        }
    }
    false
}

pub fn set_tag_collapsed(node: &mut TagNode, full_name: &str, collapsed: bool) -> bool {
    for tag in &mut node.children {
        if tag.full_name == full_name {
            tag.collapsed = collapsed;
            return true;
        }
        if set_tag_collapsed(tag, full_name, collapsed) {
            return true;
        }
    }
    false
}

/// Full deck names, depth first (completion and the change-deck prompt).
pub fn deck_names(decks: &[SidebarDeck], out: &mut Vec<String>) {
    for deck in decks {
        out.push(deck.full_name.clone());
        deck_names(&deck.children, out);
    }
}

pub fn tag_names(node: &TagNode, out: &mut Vec<String>) {
    for tag in &node.children {
        out.push(tag.full_name.clone());
        tag_names(tag, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tanki_core::{FieldInfo, NotetypeInfo, TemplateInfo};

    #[test]
    fn sections_searches_and_collapse() {
        let data = SidebarData {
            saved_searches: vec![("Mine".into(), "tag:x".into())],
            decks: vec![SidebarDeck {
                id: 5,
                name: "Lang".into(),
                full_name: "Lang".into(),
                collapsed: false,
                filtered: false,
                children: vec![SidebarDeck {
                    id: 6,
                    name: "Old words".into(),
                    full_name: "Lang::Old words".into(),
                    ..Default::default()
                }],
            }],
            tags: TagNode {
                children: vec![TagNode {
                    name: "a_b".into(),
                    full_name: "a_b".into(),
                    ..Default::default()
                }],
                ..Default::default()
            },
            notetypes: vec![NotetypeInfo {
                id: 9,
                name: "Basic (x)".into(),
                fields: vec![FieldInfo {
                    name: "Front Side".into(),
                    ..Default::default()
                }],
                templates: vec![TemplateInfo {
                    ord: 0,
                    name: "Card 1".into(),
                }],
                ..Default::default()
            }],
            flag_names: vec!["Red".into(); 7],
        };
        let mut local = LocalState::default();
        let all = rows(&data, &local);
        let find = |rows: &[Row], label: &str| rows.iter().find(|r| r.label == label).cloned();
        assert_eq!(all[0].label, "Saved Searches");
        assert_eq!(find(&all, "Mine").unwrap().search.unwrap(), "tag:x");
        assert_eq!(
            find(&all, "Old words").unwrap().search.unwrap(),
            "\"deck:Lang::Old words\""
        );
        assert_eq!(find(&all, "a_b").unwrap().search.unwrap(), "tag:a\\_b");
        assert_eq!(find(&all, "Overdue").unwrap().search.unwrap(), "(is:due -prop:due=0)");
        assert_eq!(find(&all, "Flags").unwrap().search.unwrap(), "-flag:0");
        assert!(find(&all, "Card 1").is_none());
        let sections: Vec<_> = all
            .iter()
            .filter(|r| r.depth == 0)
            .map(|r| r.label.as_str())
            .collect();
        assert_eq!(
            sections,
            ["Saved Searches", "Today", "Flags", "Card State", "Decks", "Note Types", "Tags"]
        );
        local.expanded_notetypes.insert(9);
        local.collapsed_sections.insert(Section::Today);
        let all = rows(&data, &local);
        assert!(find(&all, "Due Today").is_none());
        assert_eq!(
            find(&all, "Card 1").unwrap().search.unwrap(),
            "(\"note:Basic (x)\" card:1)"
        );
        assert_eq!(
            find(&all, "Front Side").unwrap().search.unwrap(),
            "(\"note:Basic (x)\" \"Front Side:_*\")"
        );
        let mut decks = data.decks.clone();
        assert!(set_deck_collapsed(&mut decks, 5, true));
        let data = SidebarData { decks, ..data };
        assert!(find(&rows(&data, &local), "Old words").is_none());
        let mut names = vec![];
        deck_names(&data.decks, &mut names);
        assert_eq!(names, ["Lang", "Lang::Old words"]);
    }
}
