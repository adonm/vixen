//! Page-backed interaction projections for headless/inspector surfaces.

use markup5ever_rcdom::{Handle, NodeData};
use vixen_api::ElementInfo;

use super::Page;
use crate::display_list::Rect;
use crate::form_submission::{
    FormEnctype, FormEntry, encode_multipart, encode_text_plain, encode_urlencoded,
    multipart_content_type,
};

/// Deterministic projection of a form submission. The Phase 6 host hooks will
/// turn this same entry-list + encoder output into network requests; headless
/// uses it now so `--submit-form` is observable without a network stack.
#[derive(Debug, Clone)]
pub struct FormSubmissionSnapshot {
    pub form: ElementInfo,
    pub action: String,
    pub method: String,
    pub enctype: String,
    pub content_type: String,
    pub entries: Vec<FormEntry>,
    pub body: Vec<u8>,
}

impl Page {
    /// Find the first element carrying the exact HTML `id` value.
    pub fn element_by_id(&self, id: &str) -> Option<ElementInfo> {
        self.query_selector_all("*")
            .ok()?
            .into_iter()
            .find(|element| element.id.as_deref() == Some(id))
    }

    /// Hit-test against the current layout boxes for a viewport.
    pub fn element_at(&self, viewport: (u32, u32), x: f64, y: f64) -> Option<ElementInfo> {
        let tree = self.layout_tree(viewport);
        tree.nodes.iter().rev().find_map(|node| {
            let node_id = node.dom_node_id?;
            rect_contains(node.rect, x as f32, y as f32).then(|| {
                let mut info = self
                    .document
                    .element_by_node_id(node_id)?
                    .into_element_info();
                info.bbox = Some((
                    node.rect.x as f64,
                    node.rect.y as f64,
                    node.rect.w as f64,
                    node.rect.h as f64,
                ));
                Some(info)
            })?
        })
    }

    /// Build the entry list and encoded body for the form with `form_id`.
    pub fn form_submission(&self, form_id: &str) -> Result<FormSubmissionSnapshot, String> {
        let form_node = find_element_by_id(&self.document.dom.document, form_id, Some("form"))
            .ok_or_else(|| format!("no form with id '{form_id}'"))?;
        let form = self
            .query_selector_all("form")
            .map_err(|e| format!("internal form selector failed: {e}"))?
            .into_iter()
            .find(|element| element.id.as_deref() == Some(form_id))
            .ok_or_else(|| format!("no form with id '{form_id}'"))?;

        let action = node_attr(&form_node, "action").unwrap_or_else(|| self.url.clone());
        let method = normalise_form_method(node_attr(&form_node, "method"));
        let enctype = node_attr(&form_node, "enctype")
            .as_deref()
            .and_then(FormEnctype::parse)
            .unwrap_or_default();
        let entries = form_entries(&form_node);
        let (content_type, body) = encode_form_entries(enctype, &entries);
        Ok(FormSubmissionSnapshot {
            form,
            action,
            method,
            enctype: enctype.mime_type().to_owned(),
            content_type,
            entries,
            body,
        })
    }
}

fn rect_contains(rect: Rect, x: f32, y: f32) -> bool {
    !rect.is_empty() && x >= rect.x && y >= rect.y && x < rect.x + rect.w && y < rect.y + rect.h
}

fn find_element_by_id(root: &Handle, id: &str, tag: Option<&str>) -> Option<Handle> {
    if let NodeData::Element { name, .. } = &root.data {
        let tag_matches = tag.is_none_or(|wanted| name.local.as_ref() == wanted);
        if tag_matches && node_attr(root, "id").as_deref() == Some(id) {
            return Some(root.clone());
        }
    }
    let children: Vec<Handle> = root.children.borrow().clone();
    for child in children {
        if let Some(found) = find_element_by_id(&child, id, tag) {
            return Some(found);
        }
    }
    None
}

fn node_tag(node: &Handle) -> Option<&str> {
    match &node.data {
        NodeData::Element { name, .. } => Some(name.local.as_ref()),
        _ => None,
    }
}

fn node_attr(node: &Handle, name: &str) -> Option<String> {
    let NodeData::Element { attrs, .. } = &node.data else {
        return None;
    };
    attrs
        .borrow()
        .iter()
        .find(|attr| attr.name.local.as_ref() == name)
        .map(|attr| attr.value.to_string())
}

fn has_attr(node: &Handle, name: &str) -> bool {
    node_attr(node, name).is_some()
}

fn normalise_form_method(method: Option<String>) -> String {
    match method
        .as_deref()
        .unwrap_or("get")
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "post" => "post".to_owned(),
        "dialog" => "dialog".to_owned(),
        _ => "get".to_owned(),
    }
}

fn encode_form_entries(enctype: FormEnctype, entries: &[FormEntry]) -> (String, Vec<u8>) {
    match enctype {
        FormEnctype::Urlencoded => (
            FormEnctype::Urlencoded.mime_type().to_owned(),
            encode_urlencoded(entries).into_bytes(),
        ),
        FormEnctype::TextPlain => (
            FormEnctype::TextPlain.mime_type().to_owned(),
            encode_text_plain(entries).into_bytes(),
        ),
        FormEnctype::MultipartFormData => {
            const BOUNDARY: &str = "----vixenformboundary0000000000000000";
            (
                multipart_content_type(BOUNDARY),
                encode_multipart(entries, BOUNDARY),
            )
        }
    }
}

fn form_entries(form: &Handle) -> Vec<FormEntry> {
    let mut entries = Vec::new();
    collect_form_entries(form, false, &mut entries);
    entries
}

fn collect_form_entries(node: &Handle, disabled_ancestor: bool, entries: &mut Vec<FormEntry>) {
    let tag = node_tag(node);
    let disabled_here = disabled_ancestor
        || (tag.is_some_and(is_disableable_form_element) && has_attr(node, "disabled"));

    if !disabled_here
        && let Some(tag) = tag
        && let Some(entry) = form_entry_for_control(node, tag)
    {
        entries.push(entry);
    }

    let children: Vec<Handle> = node.children.borrow().clone();
    for child in children {
        collect_form_entries(&child, disabled_here, entries);
    }
}

fn is_disableable_form_element(tag: &str) -> bool {
    matches!(
        tag,
        "button" | "fieldset" | "input" | "optgroup" | "option" | "select" | "textarea"
    )
}

fn form_entry_for_control(node: &Handle, tag: &str) -> Option<FormEntry> {
    let name = node_attr(node, "name")?;
    if name.is_empty() {
        return None;
    }
    match tag {
        "input" => input_form_entry(node, name),
        "textarea" => Some(FormEntry::text(name, node_text_content(node))),
        "select" => Some(FormEntry::text(name, selected_option_value(node))),
        // Buttons only contribute when they are the successful submitter. The
        // headless `--submit-form <id>` action has no submitter argument yet.
        "button" => None,
        _ => None,
    }
}

fn input_form_entry(node: &Handle, name: String) -> Option<FormEntry> {
    let input_type = node_attr(node, "type")
        .unwrap_or_else(|| "text".to_owned())
        .to_ascii_lowercase();
    match input_type.as_str() {
        "button" | "reset" | "submit" | "image" => None,
        "checkbox" | "radio" => has_attr(node, "checked").then(|| {
            FormEntry::text(
                name,
                node_attr(node, "value").unwrap_or_else(|| "on".to_owned()),
            )
        }),
        "file" => Some(FormEntry::file(
            name,
            node_attr(node, "value").unwrap_or_default(),
            "application/octet-stream",
            Vec::new(),
        )),
        _ => Some(FormEntry::text(
            name,
            node_attr(node, "value").unwrap_or_default(),
        )),
    }
}

fn selected_option_value(select: &Handle) -> String {
    let mut first = None;
    let mut selected = None;
    collect_options(select, &mut |option| {
        let value = node_attr(option, "value").unwrap_or_else(|| node_text_content(option));
        if first.is_none() {
            first = Some(value.clone());
        }
        if selected.is_none() && has_attr(option, "selected") {
            selected = Some(value);
        }
    });
    selected.or(first).unwrap_or_default()
}

fn collect_options<F>(node: &Handle, f: &mut F)
where
    F: FnMut(&Handle),
{
    if node_tag(node) == Some("option") {
        f(node);
    }
    let children: Vec<Handle> = node.children.borrow().clone();
    for child in children {
        collect_options(&child, f);
    }
}

fn node_text_content(node: &Handle) -> String {
    let mut out = String::new();
    collect_text(node, &mut out);
    out
}

fn collect_text(node: &Handle, out: &mut String) {
    if let NodeData::Text { contents } = &node.data {
        out.push_str(&contents.borrow());
    }
    let children: Vec<Handle> = node.children.borrow().clone();
    for child in children {
        collect_text(&child, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn element_at_uses_layout_boxes() {
        let page = Page::from_html(
            "file:///fixture.html",
            "<style>body { margin: 0; } #hit { width: 40px; height: 20px; }</style><div id='hit'>Target</div><div id='miss'>Miss</div>",
        )
        .unwrap();

        let hit = page.element_at((120, 80), 10.0, 10.0).unwrap();
        assert_eq!(hit.id.as_deref(), Some("hit"));
        assert_eq!(hit.bbox, Some((0.0, 0.0, 40.0, 20.0)));
        assert!(page.element_at((120, 80), -1.0, -1.0).is_none());
    }

    #[test]
    fn form_submission_builds_successful_controls_and_body() {
        let page = Page::from_html(
            "file:///fixture.html",
            "<form id='contact' action='/submit' method='post' enctype='application/x-www-form-urlencoded'>\
               <input name='name' value='Ada'>\
               <input name='skip' value='no' disabled>\
               <textarea name='body'>Hello, world!</textarea>\
               <select name='urgency'><option value='low'>Low</option><option value='normal' selected>Normal</option></select>\
               <input type='checkbox' name='newsletter' value='yes' checked>\
               <input type='checkbox' name='ignored' value='yes'>\
               <input type='radio' name='format' value='html' checked>\
               <input type='radio' name='format' value='text'>\
               <button type='submit' name='submitter' value='send'>Send</button>\
             </form>",
        )
        .unwrap();

        let submission = page.form_submission("contact").unwrap();
        assert_eq!(submission.method, "post");
        assert_eq!(submission.enctype, "application/x-www-form-urlencoded");
        let names: Vec<_> = submission
            .entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect();
        assert_eq!(
            names,
            vec!["name", "body", "urgency", "newsletter", "format"]
        );
        let body = String::from_utf8(submission.body).unwrap();
        assert!(body.contains("name=Ada"));
        assert!(body.contains("body=Hello%2C+world%21"));
        assert!(body.contains("urgency=normal"));
        assert!(body.contains("newsletter=yes"));
        assert!(body.contains("format=html"));
        assert!(!body.contains("skip="));
        assert!(!body.contains("submitter="));
    }
}
