//! Form controls: gives `<input>` and `<select>` the text they show (value,
//! placeholder, the chosen option, a check mark), so the ordinary layout
//! draws them as boxes with content. The UA style sheet does the rest.

use volant_core::html::{Data, Dom, Node, NodeId};

fn add(dom: &mut Dom, parent: NodeId, data: Data) -> NodeId {
    let id = dom.nodes.len() as NodeId;
    dom.nodes.push(Node { parent, children: Vec::new(), data });
    dom.nodes[parent as usize].children.push(id);
    id
}

/// The option a `<select>` shows: the selected one, else the first.
pub fn selected_option(dom: &Dom, select: NodeId) -> Option<NodeId> {
    let mut options = Vec::new();
    fn walk(dom: &Dom, id: NodeId, out: &mut Vec<NodeId>) {
        for &c in dom.children(id) {
            match dom.tag(c) {
                Some("option") => out.push(c),
                Some("optgroup") => walk(dom, c, out),
                _ => {}
            }
        }
    }
    walk(dom, select, &mut options);
    options.iter().copied().find(|&o| dom.attr(o, "selected").is_some()).or(options.first().copied())
}

/// Adds the shown text to every form control in `dom`.
pub fn prepare(dom: &mut Dom) {
    let n = dom.nodes.len() as NodeId;
    for id in 0..n {
        let Some(tag) = dom.tag(id) else { continue };
        match tag {
            "input" => {
                let ty = dom.attr(id, "type").unwrap_or("text").to_ascii_lowercase();
                let value = dom.attr(id, "value").map(str::to_string);
                let (text, placeholder) = match ty.as_str() {
                    "hidden" | "range" | "color" => continue,
                    "checkbox" => (if dom.attr(id, "checked").is_some() { "✓".to_string() } else { String::new() }, false),
                    "radio" => (if dom.attr(id, "checked").is_some() { "•".to_string() } else { String::new() }, false),
                    "submit" => (value.unwrap_or_else(|| "Submit".into()), false),
                    "reset" => (value.unwrap_or_else(|| "Reset".into()), false),
                    "button" => (value.unwrap_or_default(), false),
                    "image" => (dom.attr(id, "alt").unwrap_or("Submit").to_string(), false),
                    "file" => ("Choose File".into(), false),
                    "password" => (value.map(|v| "•".repeat(v.chars().count())).unwrap_or_default(), false),
                    _ => match value.filter(|v| !v.is_empty()) {
                        Some(v) => (v, false),
                        None => (dom.attr(id, "placeholder").unwrap_or("").to_string(), true),
                    },
                };
                if text.is_empty() {
                    // An empty field still has a line's height.
                    add(dom, id, Data::Text("\u{200b}".into()));
                } else if placeholder {
                    let span = add(dom, id, Data::Element { name: "span".into(), attrs: vec![("class".into(), "-volant-placeholder".into())] });
                    add(dom, span, Data::Text(text));
                } else {
                    add(dom, id, Data::Text(text));
                }
            }
            "select" => {
                let shown = selected_option(dom, id).map(|o| dom.text(o).trim().to_string()).unwrap_or_default();
                add(dom, id, Data::Text(format!("{shown}  ▾")));
            }
            _ => {}
        }
    }
}
