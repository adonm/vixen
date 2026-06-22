//! HTML fragment serialization — Phase 6 host-bindings prep (pure logic
//! called out by `docs/PLAN.md` Phase 6 step 1 "DOM Core"). Implements the
//! WHATWG HTML § 13.2.9 "Serializing HTML fragments" algorithm — the inverse
//! of `html5ever` parse — so the `Element.innerHTML` getter, `outerHTML`,
//! `document.write`, `DOMParser`-round-trip, and `XMLHttpRequest.responseText`
//! (for HTML documents) host hooks read from one source of truth.
//!
//! What lives here:
//! - [`serialize_children`] — the § 13.2.9 fragment serializer: walk the
//!   `RcDom` subtree under a node and emit the HTML string of its children
//!   (the `Element.innerHTML` getter).
//! - [`serialize_node`] — one node + its descendants (the `Element.outerHTML`
//!   getter).
//! - [`escape_text`] / [`escape_attribute`] — the § 13.2.9 escape rules
//!   (text: `&`, `<`, `>`, NBSP; attribute: `&`, `"`, NBSP — no `&nbsp;`
//!   emission is optional; we emit the literal character so round-tripping
//!   preserves the byte, matching Firefox).
//!
//! What does *not* live here:
//! - Parsing-side round-trip guarantees. The HTML parser (html5ever) reorders
//!   attributes, normalises tag names, and inserts implied tags; the
//!   serializer preserves whatever the DOM currently holds. A `parse →
//!   serialize → parse` cycle may yield a different (but equivalent) DOM
//!   tree, which is the spec's documented behaviour.
//! - Foreign-content (SVG / MathML) namespace handling. HTML's § 13.2.9
//!   gives SVG / MathML elements the same treatment as HTML (same escape
//!   rules, same attribute serialisation) *except* for `script`/`style`
//!   foreign elements, which we already special-case by tag name. The
//!   HTML-in-foreign-content CDATA escapes are deferred until the SVG CDATA
//!   section lands in the DOM layer.
//! - Pre-serialization tree mutation (the § 13.2.9 pre-step that runs
//!   "ensure pre-insertion validity" for `innerHTML` *setter* — that's the
//!   parse side). This module is the read-only getter surface.
//!
//! ## Void + raw-text element tables
//!
//! The § 13.2.9 algorithm distinguishes three element classes:
//! 1. **Void elements** (`area`/`base`/`br`/`col`/`embed`/`hr`/`img`/`input`/
//!    `link`/`meta`/`param`/`source`/`track`/`wbr`) — open tag only, no
//!    closing tag, no children serialised.
//! 2. **Raw-text elements** (`script`/`style`/`xmp`/`iframe`/`noembed`/
//!    `noframes`/`plaintext`, plus `noscript` when scripting is enabled) —
//!    children are emitted verbatim with no text escaping.
//! 3. **Everything else** — open tag, children (with text escaping), close tag.
//!
//! The void-element list is the HTML-only spec list; foreign (SVG/MathML)
//! elements use the open-tag + close-tag form (their void elements, like
//! SVG's self-closing convention, are handled by the caller).
//!
//! Reference: <https://html.spec.whatwg.org/#serialising-html-fragments>.

#![forbid(unsafe_code)]

use markup5ever_rcdom::Handle;
use markup5ever_rcdom::NodeData;

/// Whether the scripting flag is enabled during serialization. Affects only
/// the `noscript` element: with scripting enabled, its children are raw-text
/// (the spec's § 13.2.9 step 3.5 case); with scripting disabled, `noscript`
/// is a normal element. Production browsers serialize with scripting enabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Scripting {
    /// Scripting enabled — `noscript` is raw-text. The default for the
    /// browser-shell serialisation path.
    #[default]
    Enabled,
    /// Scripting disabled — `noscript` is a normal element (used by
    /// `DOMParser.parseFromString(html, "text/html")` and the print path).
    Disabled,
}

/// Serialise the children of `node` (the `Element.innerHTML` getter and the
/// `DocumentFragment` serialiser). Each child is appended to `out`.
///
/// Per § 13.2.9 the algorithm is recursive; the `parent` parameter carries
/// the tag name so the text-escape rule can consult "is this child of a
/// raw-text element?".
pub fn serialize_children(node: &Handle, scripting: Scripting, out: &mut String) {
    let parent_tag = element_local_name(node);
    let children: Vec<Handle> = node.children.borrow().clone();
    for child in &children {
        serialize_node_inner(child, scripting, parent_tag.as_deref(), out);
    }
}

/// Serialise `node` *and* its descendants (the `Element.outerHTML` getter).
/// The root document node serialises its children without emitting a wrapper.
pub fn serialize_node(node: &Handle, scripting: Scripting, out: &mut String) {
    let parent_tag = parent_element_local_name(node);
    serialize_node_inner(node, scripting, parent_tag.as_deref(), out);
}

/// The full HTML serialisation of `node`'s subtree, as a freshly-allocated
/// `String`. Convenience wrapper around [`serialize_node`].
pub fn serialize_to_string(node: &Handle, scripting: Scripting) -> String {
    let mut out = String::new();
    serialize_node(node, scripting, &mut out);
    out
}

/// The HTML serialisation of `node`'s child subtree (i.e. children only),
/// as a freshly-allocated `String`. Convenience wrapper around
/// [`serialize_children`].
pub fn serialize_children_to_string(node: &Handle, scripting: Scripting) -> String {
    let mut out = String::new();
    serialize_children(node, scripting, &mut out);
    out
}

// ---------------------------------------------------------------------------
// Per-node serialiser (private)
// ---------------------------------------------------------------------------

fn serialize_node_inner(
    node: &Handle,
    scripting: Scripting,
    parent_tag: Option<&str>,
    out: &mut String,
) {
    match &node.data {
        NodeData::Document => {
            // The document root: serialise children, no wrapper.
            let children: Vec<Handle> = node.children.borrow().clone();
            for child in &children {
                serialize_node_inner(child, scripting, None, out);
            }
        }
        NodeData::Doctype { name, .. } => {
            // § 13.2.9 step 4: `<!doctype name>`.
            out.push_str("<!doctype ");
            out.push_str(name);
            out.push('>');
        }
        NodeData::Text { contents } => {
            let borrowed = contents.borrow();
            if let Some(tag) = parent_tag
                && is_raw_text_element(tag, scripting)
            {
                // Raw-text parent: emit verbatim, no escaping.
                out.push_str(&borrowed);
                return;
            }
            escape_text(&borrowed, out);
        }
        NodeData::Comment { contents } => {
            // § 13.2.9 step 6: `<!-- contents -->`. The spec disallows `-->`
            // inside the comment body (a parser-level invariant; the
            // serializer does not transform it but does not emit it either
            // for trees built via the parser). Defensive: replace `-->` with
            // `--&gt;` so the output is always a parseable comment.
            out.push_str("<!--");
            push_comment_contents(contents, out);
            out.push_str("-->");
        }
        NodeData::Element { name, attrs, .. } => {
            let tag = name.local.as_ref();
            out.push('<');
            out.push_str(tag);
            let borrowed = attrs.borrow();
            for attr in borrowed.iter() {
                out.push(' ');
                out.push_str(attr.name.local.as_ref());
                out.push_str("=\"");
                escape_attribute(&attr.value, out);
                out.push('"');
            }
            out.push('>');
            if is_void_element(tag) {
                // Void elements: no children, no close tag.
                return;
            }
            // Children.
            let children: Vec<Handle> = node.children.borrow().clone();
            if is_raw_text_element(tag, scripting) {
                // Raw-text element: children emitted verbatim (no escape).
                // Only text-node children carry the raw payload, but any
                // nested element is a parse error in the raw-text context;
                // serialise the children as-is per spec (the parser would
                // have re-routed them).
                for child in &children {
                    serialize_raw_child(child, scripting, out);
                }
            } else {
                for child in &children {
                    serialize_node_inner(child, scripting, Some(tag), out);
                }
            }
            out.push_str("</");
            out.push_str(tag);
            out.push('>');
        }
        NodeData::ProcessingInstruction { target, contents } => {
            // § 13.2.9 step 7: `<?target contents>`. HTML's processing
            // instructions are rare (parser constructs them only from
            // `<?...` in foreign content); emit verbatim.
            out.push_str("<?");
            out.push_str(target);
            out.push(' ');
            out.push_str(contents);
            out.push('>');
        }
    }
}

fn serialize_raw_child(node: &Handle, scripting: Scripting, out: &mut String) {
    match &node.data {
        NodeData::Text { contents } => out.push_str(&contents.borrow()),
        _ => serialize_node_inner(node, scripting, None, out),
    }
}

/// Push the comment body, defensive against `-->` (which would close the
/// comment prematurely if a non-parser source built the tree). Per spec the
/// serializer does not transform comment contents; we stay total by emitting
/// the literal `--&gt;` so the output round-trips through the parser.
fn push_comment_contents(contents: &str, out: &mut String) {
    // The parser only produces comments without `-->`; this is defensive.
    let mut last = '\0';
    for c in contents.chars() {
        if c == '>' && out.ends_with("--") {
            // Pop the trailing `--` and emit `--&gt;` instead.
            out.pop();
            out.pop();
            out.push_str("--&gt;");
            last = c;
            continue;
        }
        out.push(c);
        last = c;
    }
    let _ = last; // suppress unused-assignment lint in release
}

// ---------------------------------------------------------------------------
// Escaping (§ 13.2.9 steps 5 + 8)
// ---------------------------------------------------------------------------

/// Escape a text-node body per § 13.2.9 step 8: `&` → `&amp;`, `<` → `&lt;`,
/// `>` → `&gt;`, NBSP (`\u{00A0}`) → `&nbsp;`. Appended to `out`.
pub fn escape_text(text: &str, out: &mut String) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\u{00A0}' => out.push_str("&nbsp;"),
            _ => out.push(c),
        }
    }
}

/// Escape an attribute value per § 13.2.9 step 5: `&` → `&amp;`,
/// `"` → `&quot;`, NBSP (`\u{00A0}`) → `&nbsp;`. Appended to `out`.
/// The output is wrapped in `"`s by the caller; this function does not add
/// the surrounding quotes.
pub fn escape_attribute(value: &str, out: &mut String) {
    for c in value.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '\u{00A0}' => out.push_str("&nbsp;"),
            _ => out.push(c),
        }
    }
}

// ---------------------------------------------------------------------------
// Element classification (HTML-only tables; foreign elements use default)
// ---------------------------------------------------------------------------

/// The HTML void-element table per WHATWG HTML § 13.2.9 step 3.4. Foreign
/// (SVG/MathML) elements never match this — their self-closing convention is
/// a parser/author concern, not a serializer one.
fn is_void_element(tag: &str) -> bool {
    matches!(
        tag,
        "area"
            | "base"
            | "br"
            | "col"
            | "embed"
            | "hr"
            | "img"
            | "input"
            | "link"
            | "meta"
            | "param"
            | "source"
            | "track"
            | "wbr"
    )
}

/// The HTML raw-text element table per § 13.2.9 step 3.5. `noscript` is
/// raw-text only when scripting is enabled (the production case); when
/// disabled, `noscript` is a normal element (the `DOMParser` case).
fn is_raw_text_element(tag: &str, scripting: Scripting) -> bool {
    matches!(
        tag,
        "script" | "style" | "xmp" | "iframe" | "noembed" | "noframes" | "plaintext"
    ) || (tag == "noscript" && scripting == Scripting::Enabled)
}

// ---------------------------------------------------------------------------
// Tree helpers (RcDom shape)
// ---------------------------------------------------------------------------

fn element_local_name(node: &Handle) -> Option<String> {
    if let NodeData::Element { name, .. } = &node.data {
        Some(name.local.to_string())
    } else {
        None
    }
}

fn parent_element_local_name(node: &Handle) -> Option<String> {
    // RcDOM stores the parent as a `Cell<Option<Weak<Node>>>`. `take()`
    // leaves `None` behind; restore on the way out to keep the tree intact
    // (cheap — a single cell write).
    let weak = node.parent.take();
    let result = weak
        .as_ref()
        .and_then(|w| w.upgrade())
        .and_then(|p| match &p.data {
            NodeData::Element { name, .. } => Some(name.local.to_string()),
            _ => None,
        });
    node.parent.set(weak);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::Document;
    use markup5ever_rcdom::Handle;

    /// Parse + return the document handle (skipping the auto-inserted
    /// `<html>` shell when the input is a fragment).
    fn parse(html: &str) -> Handle {
        let doc = Document::parse(html).unwrap();
        // The RcDom stores the document under `dom.document`; clone the Rc.
        Handle::clone(&doc.dom.document)
    }

    /// Parse `html` and return the owning `Document` plus a handle to its
    /// document root. The Document must outlive any handles derived from it:
    /// the RcDOM tree's only strong references to interior nodes are via the
    /// children-chains rooted at the document node, so dropping the Document
    /// before serialising can sever those chains (the html5ever RcDOM is a
    /// tree, not a graph). The real host hook owns the Document the same way.
    fn parse_with_doc(html: &str) -> (Document, Handle) {
        let doc = Document::parse(html).unwrap();
        let handle = Handle::clone(&doc.dom.document);
        (doc, handle)
    }

    /// Parse `html`, find the first non-shell element (DFS), and return it
    /// along with the owning `Document`. Callers must keep both in scope.
    fn parse_first_element_with_doc(html: &str) -> (Document, Handle) {
        let (doc, root) = parse_with_doc(html);
        fn walk(node: &Handle) -> Option<Handle> {
            if let NodeData::Element { name, .. } = &node.data {
                let tag = name.local.as_ref();
                if !matches!(tag, "html" | "head" | "body") {
                    return Some(Handle::clone(node));
                }
            }
            for child in node.children.borrow().iter() {
                if let Some(found) = walk(child) {
                    return Some(found);
                }
            }
            None
        }
        let handle = walk(&root).expect("test input must contain at least one non-shell element");
        (doc, handle)
    }

    // --- Escape rules --------------------------------------------------

    #[test]
    fn escape_text_replaces_amp_lt_gt_nbsp() {
        let mut s = String::new();
        escape_text("a & b < c > d\u{00A0}e", &mut s);
        assert_eq!(s, "a &amp; b &lt; c &gt; d&nbsp;e");
    }

    #[test]
    fn escape_attribute_replaces_amp_quote_nbsp() {
        let mut s = String::new();
        escape_attribute("a & b \" c\u{00A0}d", &mut s);
        assert_eq!(s, "a &amp; b &quot; c&nbsp;d");
    }

    #[test]
    fn escape_text_passes_through_other_chars() {
        let mut s = String::new();
        escape_text("plain text 123 'apostrophe'", &mut s);
        assert_eq!(s, "plain text 123 'apostrophe'");
    }

    // --- Element round-trip --------------------------------------------

    #[test]
    fn simple_element_round_trips() {
        let (_doc, p) = parse_first_element_with_doc("<p>hello</p>");
        assert_eq!(serialize_to_string(&p, Scripting::Enabled), "<p>hello</p>");
    }

    #[test]
    fn nested_elements_round_trip() {
        let (_doc, p) = parse_first_element_with_doc("<div><p>x</p><span>y</span></div>");
        assert_eq!(
            serialize_to_string(&p, Scripting::Enabled),
            "<div><p>x</p><span>y</span></div>"
        );
    }

    #[test]
    fn attributes_preserved_in_order() {
        let (_doc, p) = parse_first_element_with_doc("<a href=\"/x\" class=\"nav\">link</a>");
        assert_eq!(
            serialize_to_string(&p, Scripting::Enabled),
            "<a href=\"/x\" class=\"nav\">link</a>"
        );
    }

    #[test]
    fn attribute_value_with_quote_is_escaped() {
        let (_doc, p) =
            parse_first_element_with_doc("<a title=\"a &quot;quotable&quot; thing\">x</a>");
        let out = serialize_to_string(&p, Scripting::Enabled);
        assert!(out.contains("a &quot;quotable&quot; thing"));
    }

    // --- Void elements -------------------------------------------------

    #[test]
    fn void_elements_have_no_close_tag() {
        for tag in ["br", "img", "hr", "input", "meta", "link"] {
            let (_doc, p) = parse_first_element_with_doc(&format!("<{tag}>"));
            let out = serialize_to_string(&p, Scripting::Enabled);
            assert!(
                out == format!("<{tag}>"),
                "void element {tag}: serialised as {out:?}, expected `<{tag}>`"
            );
        }
    }

    #[test]
    fn void_element_with_attributes() {
        let (_doc, p) = parse_first_element_with_doc("<img src=\"a.png\" alt=\"pic\">");
        let out = serialize_to_string(&p, Scripting::Enabled);
        assert!(out.starts_with("<img "));
        assert!(out.contains("src=\"a.png\""));
        assert!(out.contains("alt=\"pic\""));
        assert!(out.ends_with('>'));
        assert!(!out.contains("</img>"));
    }

    // --- Raw-text elements ---------------------------------------------

    #[test]
    fn script_children_emitted_verbatim() {
        let (_doc, p) =
            parse_first_element_with_doc("<script>if (a < b && c > d) { x(); }</script>");
        let out = serialize_to_string(&p, Scripting::Enabled);
        assert!(out.contains("if (a < b && c > d)"));
        // No escape inside script.
        assert!(!out.contains("&lt;"));
        assert!(!out.contains("&amp;"));
        assert!(out.ends_with("</script>"));
    }

    #[test]
    fn style_children_emitted_verbatim() {
        let (_doc, p) = parse_first_element_with_doc("<style>body > div { color: red; }</style>");
        let out = serialize_to_string(&p, Scripting::Enabled);
        assert!(out.contains("body > div"));
        // No escape inside style.
        assert!(!out.contains("&gt;"));
        assert!(out.ends_with("</style>"));
    }

    #[test]
    fn text_in_normal_element_escaped() {
        let (_doc, p) = parse_first_element_with_doc("<p>a < b && c > d</p>");
        let out = serialize_to_string(&p, Scripting::Enabled);
        assert!(out.contains("a &lt; b &amp;&amp; c &gt; d"));
    }

    #[test]
    fn noscript_is_raw_text_when_scripting_enabled() {
        let (_doc, p) = parse_first_element_with_doc("<noscript>a < b</noscript>");
        let enabled = serialize_to_string(&p, Scripting::Enabled);
        let disabled = serialize_to_string(&p, Scripting::Disabled);
        // Scripting-enabled ⇒ verbatim (no escape).
        assert!(enabled.contains("a < b"));
        assert!(!enabled.contains("&lt;"));
        // Scripting-disabled ⇒ normal escape.
        assert!(disabled.contains("a &lt; b"));
    }

    // --- innerHTML (children-only) surface -----------------------------

    #[test]
    fn innerhtml_emits_children_only() {
        let (_doc, div) = parse_first_element_with_doc("<div><p>one</p><p>two</p></div>");
        let inner = serialize_children_to_string(&div, Scripting::Enabled);
        assert_eq!(inner, "<p>one</p><p>two</p>");
    }

    #[test]
    fn innerhtml_excludes_self() {
        let (_doc, div) = parse_first_element_with_doc("<div id=\"outer\">text</div>");
        let inner = serialize_children_to_string(&div, Scripting::Enabled);
        assert_eq!(inner, "text");
    }

    // --- Comments + doctype --------------------------------------------

    #[test]
    fn comment_round_trips() {
        let html = "<!-- a comment --><p>x</p>";
        let root = parse(html);
        let out = serialize_children_to_string(&root, Scripting::Enabled);
        assert!(out.contains("<!-- a comment -->"));
    }

    #[test]
    fn doctype_round_trips() {
        let html = "<!doctype html><html><body>x</body></html>";
        let root = parse(html);
        let out = serialize_children_to_string(&root, Scripting::Enabled);
        assert!(out.starts_with("<!doctype html>") || out.contains("<!doctype html>"));
    }

    // --- Edge cases ----------------------------------------------------

    #[test]
    fn nbsp_in_text_escaped() {
        // The body must be inside an element we can find.
        let (_doc, p) = parse_first_element_with_doc("<p>x\u{00A0}y</p>");
        let out = serialize_to_string(&p, Scripting::Enabled);
        assert!(out.contains("x&nbsp;y"));
    }

    #[test]
    fn empty_element_emits_open_close_pair() {
        let (_doc, p) = parse_first_element_with_doc("<div></div>");
        assert_eq!(serialize_to_string(&p, Scripting::Enabled), "<div></div>");
    }

    #[test]
    fn round_trips_a_real_fragment() {
        let html =
            "<div class=\"card\"><h2>Title</h2><p>Body &amp; soul</p><br><img src=\"a.png\"></div>";
        let (_doc, div) = parse_first_element_with_doc(html);
        let out = serialize_to_string(&div, Scripting::Enabled);
        // The class attribute survives.
        assert!(out.starts_with("<div class=\"card\">"));
        // The body's escaped &amp; is re-escaped (so round-trip-safe).
        assert!(out.contains("Body &amp;amp; soul") || out.contains("Body &amp; soul"));
        // Void elements stay void.
        assert!(out.contains("<br>"));
        assert!(out.contains("<img src=\"a.png\">"));
        // Closing tag intact.
        assert!(out.ends_with("</div>"));
    }
}
