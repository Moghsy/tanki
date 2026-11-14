//! Conversions shared by the Phase 3 core streams (lead-owned).
use crate::*;
use anki::{collection::Collection, services::NotetypesService};
use anki_proto as pb;

/// `NotetypeInfo` for one note type (`GetNotetype` + `GetClozeFieldOrds`).
pub(crate) fn notetype_info(col: &mut Collection, id: i64) -> Result<NotetypeInfo> {
    use pb::notetypes::notetype::config::Kind;
    use pb::notetypes::stock_notetype::OriginalStockKind;
    let nt = NotetypesService::get_notetype(col, pb::notetypes::NotetypeId { ntid: id })?;
    let config = nt.config.unwrap_or_default();
    let kind = if config.kind == Kind::Cloze as i32 {
        NotetypeKind::Cloze
    } else {
        NotetypeKind::Normal
    };
    let cloze_fields = if kind == NotetypeKind::Cloze {
        NotetypesService::get_cloze_field_ords(col, pb::notetypes::NotetypeId { ntid: id })?.ords
    } else {
        Vec::new()
    };
    let fields = nt
        .fields
        .into_iter()
        .enumerate()
        .map(|(index, f)| {
            let c = f.config.unwrap_or_default();
            FieldInfo {
                ord: f.ord.map_or(index as u32, |o| o.val),
                name: f.name,
                sticky: c.sticky,
                rtl: c.rtl,
                plain_text: c.plain_text,
                collapsed: c.collapsed,
                description: c.description,
                exclude_from_search: c.exclude_from_search,
                prevent_deletion: c.prevent_deletion,
            }
        })
        .collect();
    let templates = nt
        .templates
        .into_iter()
        .enumerate()
        .map(|(index, t)| TemplateInfo {
            ord: t.ord.map_or(index as u32, |o| o.val),
            name: t.name,
        })
        .collect();
    Ok(NotetypeInfo {
        id: nt.id,
        name: nt.name,
        kind,
        fields,
        templates,
        sort_field: config.sort_field_idx,
        cloze_fields,
        image_occlusion: config.original_stock_kind == OriginalStockKind::ImageOcclusion as i32,
    })
}

/// All note types, sorted case-insensitively by name.
pub(crate) fn all_notetypes(col: &mut Collection) -> Result<Vec<NotetypeInfo>> {
    let names = NotetypesService::get_notetype_names(col)?;
    let mut all = names
        .entries
        .into_iter()
        .map(|entry| notetype_info(col, entry.id))
        .collect::<Result<Vec<_>>>()?;
    all.sort_by_key(|nt| nt.name.to_lowercase());
    Ok(all)
}

/// `TagTreeNode` with full `::` names filled in.
pub(crate) fn tag_node(node: pb::tags::TagTreeNode, parent: &str) -> TagNode {
    let full_name = match (parent.is_empty(), node.name.is_empty()) {
        (_, true) => String::new(),
        (true, false) => node.name.clone(),
        (false, false) => format!("{parent}::{}", node.name),
    };
    TagNode {
        children: node
            .children
            .into_iter()
            .map(|child| tag_node(child, &full_name))
            .collect(),
        name: node.name,
        full_name,
        collapsed: node.collapsed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stock_notetypes_and_tag_paths() {
        let dir = tempfile::tempdir().unwrap();
        let mut col = anki::collection::CollectionBuilder::new(dir.path().join("c.anki2"))
            .build()
            .unwrap();
        let all = all_notetypes(&mut col).unwrap();
        let basic = all.iter().find(|nt| nt.name == "Basic").unwrap();
        assert_eq!(basic.kind, NotetypeKind::Normal);
        assert_eq!(
            basic
                .fields
                .iter()
                .map(|f| f.name.as_str())
                .collect::<Vec<_>>(),
            ["Front", "Back"]
        );
        let cloze = all.iter().find(|nt| nt.name == "Cloze").unwrap();
        assert_eq!(cloze.kind, NotetypeKind::Cloze);
        assert_eq!(cloze.cloze_fields, [0]);
        assert!(all.iter().any(|nt| nt.image_occlusion));
        let tree = pb::tags::TagTreeNode {
            name: String::new(),
            children: vec![pb::tags::TagTreeNode {
                name: "a".into(),
                children: vec![pb::tags::TagTreeNode {
                    name: "b".into(),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };
        let node = tag_node(tree, "");
        assert_eq!(node.full_name, "");
        assert_eq!(node.children[0].children[0].full_name, "a::b");
        col.close(None).unwrap();
    }
}
