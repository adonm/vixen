//! Page-backed interaction projections for headless/inspector surfaces.

use std::rc::Rc;

use markup5ever_rcdom::{Handle, NodeData};
use vixen_api::ElementInfo;

use super::Page;
use crate::display_list::Rect;
use crate::form_submission::{
    FormEnctype, FormEntry, encode_multipart, encode_text_plain, encode_urlencoded,
    multipart_content_type,
};

/// Page-owned selection state that survives runtime realm replacement.
///
/// The current DOM bridge only gives parsed elements stable positive node ids,
/// so this state deliberately covers document/element boundary points. Text
/// node boundary persistence remains unsupported rather than being restored to
/// the wrong node after a structural mutation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageSelection {
    pub anchor_node_id: usize,
    pub anchor_offset: usize,
    pub focus_node_id: usize,
    pub focus_offset: usize,
}

/// Deterministic projection of a form submission. The Phase 6 host hooks will
/// turn this same entry-list + encoder output into network requests; headless
/// uses it now so `--submit-form` is observable without a network stack.
#[derive(Debug, Clone)]
pub struct FormSubmissionSnapshot {
    pub form: ElementInfo,
    pub submitter: Option<ElementInfo>,
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
        let form = self
            .query_selector_all("form")
            .map_err(|e| format!("internal form selector failed: {e}"))?
            .into_iter()
            .find(|element| element.id.as_deref() == Some(form_id))
            .ok_or_else(|| format!("no form with id '{form_id}'"))?;

        self.form_submission_by_node_id(form.node_id, None)
    }

    /// Build the entry list and encoded body for a form by stable page node id.
    ///
    /// This is the browser-real submission seam used by runtime/CDP actions:
    /// idless forms still submit, and a successful submitter can override
    /// `action`/`method`/`enctype` and contribute its own name/value entry.
    pub fn form_submission_by_node_id(
        &self,
        form_node_id: usize,
        submitter_node_id: Option<usize>,
    ) -> Result<FormSubmissionSnapshot, String> {
        let form_node = find_element_by_node_id(&self.document.dom.document, form_node_id)
            .ok_or_else(|| format!("no form node {form_node_id}"))?;
        if node_tag(&form_node) != Some("form") {
            return Err(format!("node {form_node_id} is not a form"));
        }
        let form = self
            .document
            .element_by_node_id(form_node_id)
            .ok_or_else(|| format!("no form node {form_node_id}"))?
            .into_element_info();

        let submitter_node = submitter_node_id
            .filter(|node_id| *node_id != 0)
            .map(|node_id| {
                find_element_by_node_id(&self.document.dom.document, node_id)
                    .ok_or_else(|| format!("no submitter node {node_id}"))
            })
            .transpose()?;
        if let Some(submitter) = &submitter_node {
            if !is_submitter_control(submitter) {
                return Err(format!(
                    "node {} is not a submit button",
                    submitter_node_id.unwrap_or_default()
                ));
            }
            if !node_contains(&form_node, submitter) {
                return Err(format!(
                    "submitter node {} is not owned by form node {form_node_id}",
                    submitter_node_id.unwrap_or_default()
                ));
            }
        }
        let submitter = submitter_node_id
            .filter(|node_id| *node_id != 0)
            .and_then(|node_id| self.document.element_by_node_id(node_id))
            .map(|element| element.into_element_info());

        let raw_action = submitter_node
            .as_ref()
            .and_then(|node| node_attr(node, "formaction"))
            .filter(|action| !action.is_empty())
            .or_else(|| node_attr(&form_node, "action"))
            .filter(|action| !action.is_empty())
            .unwrap_or_else(|| self.url.clone());
        let action = self.resolve_url(&raw_action).unwrap_or(raw_action);
        let method = normalise_form_method(
            submitter_node
                .as_ref()
                .and_then(|node| node_attr(node, "formmethod"))
                .or_else(|| node_attr(&form_node, "method")),
        );
        let enctype = submitter_node
            .as_ref()
            .and_then(|node| node_attr(node, "formenctype"))
            .or_else(|| node_attr(&form_node, "enctype"))
            .as_deref()
            .and_then(FormEnctype::parse)
            .unwrap_or_default();
        let entries = form_entries(&form_node, submitter_node.as_ref());
        let (content_type, body) = encode_form_entries(enctype, &entries);
        Ok(FormSubmissionSnapshot {
            form,
            submitter,
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

fn find_element_by_node_id(root: &Handle, node_id: usize) -> Option<Handle> {
    let mut current = 0;
    find_element_by_node_id_inner(root, node_id, &mut current)
}

fn find_element_by_node_id_inner(
    root: &Handle,
    node_id: usize,
    current: &mut usize,
) -> Option<Handle> {
    if matches!(root.data, NodeData::Element { .. }) {
        *current += 1;
        if *current == node_id {
            return Some(root.clone());
        }
    }

    let children: Vec<Handle> = root.children.borrow().clone();
    for child in children {
        if let Some(found) = find_element_by_node_id_inner(&child, node_id, current) {
            return Some(found);
        }
    }
    None
}

fn node_contains(root: &Handle, candidate: &Handle) -> bool {
    if Rc::ptr_eq(root, candidate) {
        return true;
    }
    let children: Vec<Handle> = root.children.borrow().clone();
    children.iter().any(|child| node_contains(child, candidate))
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

fn form_entries(form: &Handle, submitter: Option<&Handle>) -> Vec<FormEntry> {
    let mut entries = Vec::new();
    collect_form_entries(form, false, submitter, &mut entries);
    entries
}

fn collect_form_entries(
    node: &Handle,
    disabled_ancestor: bool,
    submitter: Option<&Handle>,
    entries: &mut Vec<FormEntry>,
) {
    let tag = node_tag(node);
    let disabled_here = disabled_ancestor
        || (tag.is_some_and(is_disableable_form_element) && has_attr(node, "disabled"));

    if !disabled_here
        && let Some(tag) = tag
        && let Some(entry) =
            form_entry_for_control(node, tag, is_successful_submitter(node, submitter))
    {
        entries.push(entry);
    }

    let children: Vec<Handle> = node.children.borrow().clone();
    for child in children {
        collect_form_entries(&child, disabled_here, submitter, entries);
    }
}

fn is_disableable_form_element(tag: &str) -> bool {
    matches!(
        tag,
        "button" | "fieldset" | "input" | "optgroup" | "option" | "select" | "textarea"
    )
}

fn is_successful_submitter(node: &Handle, submitter: Option<&Handle>) -> bool {
    submitter.is_some_and(|submitter| Rc::ptr_eq(node, submitter))
}

fn is_submitter_control(node: &Handle) -> bool {
    match node_tag(node) {
        Some("button") => matches!(
            node_attr(node, "type")
                .unwrap_or_else(|| "submit".to_owned())
                .to_ascii_lowercase()
                .as_str(),
            "" | "submit"
        ),
        Some("input") => matches!(
            node_attr(node, "type")
                .unwrap_or_else(|| "text".to_owned())
                .to_ascii_lowercase()
                .as_str(),
            "submit" | "image"
        ),
        _ => false,
    }
}

fn form_entry_for_control(
    node: &Handle,
    tag: &str,
    successful_submitter: bool,
) -> Option<FormEntry> {
    let name = node_attr(node, "name")?;
    if name.is_empty() {
        return None;
    }
    match tag {
        "input" => input_form_entry(node, name, successful_submitter),
        "textarea" => Some(FormEntry::text(name, node_text_content(node))),
        "select" => Some(FormEntry::text(name, selected_option_value(node))),
        "button" => button_form_entry(node, name, successful_submitter),
        _ => None,
    }
}

fn input_form_entry(node: &Handle, name: String, successful_submitter: bool) -> Option<FormEntry> {
    let input_type = node_attr(node, "type")
        .unwrap_or_else(|| "text".to_owned())
        .to_ascii_lowercase();
    match input_type.as_str() {
        "button" | "reset" => None,
        "submit" => successful_submitter
            .then(|| FormEntry::text(name, node_attr(node, "value").unwrap_or_default())),
        "image" => successful_submitter.then(|| {
            // Pointer coordinates are not carried by the current click action;
            // submit the deterministic origin coordinate pair for this narrow seam.
            FormEntry::text(format!("{name}.x"), "0")
        }),
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

fn button_form_entry(node: &Handle, name: String, successful_submitter: bool) -> Option<FormEntry> {
    if !successful_submitter {
        return None;
    }
    let button_type = node_attr(node, "type")
        .unwrap_or_else(|| "submit".to_owned())
        .to_ascii_lowercase();
    (button_type == "submit" || button_type.is_empty())
        .then(|| FormEntry::text(name, node_attr(node, "value").unwrap_or_default()))
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
        assert!(submission.submitter.is_none());
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

    #[test]
    fn form_submission_by_node_id_honors_submitter_overrides() {
        let page = Page::from_html(
            "file:///forms/index.html",
            "<form action='/default' method='post' enctype='text/plain'>\
               <input name='q' value='rust'>\
               <button id='go' name='submitter' value='send' formaction='next.html' formmethod='get' formenctype='application/x-www-form-urlencoded'>Go</button>\
             </form>",
        )
        .unwrap();
        let form = page.query_selector_all("form").unwrap()[0].clone();
        let button = page.query_selector_all("#go").unwrap()[0].clone();

        let submission = page
            .form_submission_by_node_id(form.node_id, Some(button.node_id))
            .unwrap();

        assert_eq!(submission.form.node_id, form.node_id);
        assert_eq!(
            submission.submitter.as_ref().map(|s| s.node_id),
            Some(button.node_id)
        );
        assert_eq!(submission.action, "file:///forms/next.html");
        assert_eq!(submission.method, "get");
        assert_eq!(submission.enctype, "application/x-www-form-urlencoded");
        let names: Vec<_> = submission
            .entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect();
        assert_eq!(names, vec!["q", "submitter"]);
        assert_eq!(
            String::from_utf8(submission.body).unwrap(),
            "q=rust&submitter=send"
        );
    }
}
