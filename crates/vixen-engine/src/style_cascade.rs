//! First executable CSS cascade slice behind [`crate::page::Page`].
//!
//! This is deliberately smaller than the final Stylo style system, but it moves
//! Vixen past inline-only style projection: `<style>` blocks are parsed in
//! document order, selectors are matched through the existing Stylo selector
//! adapter, declarations cascade by importance → origin tier → cascade layer →
//! specificity → source order, and inline `style` attributes still sit on the
//! same computed style surface. The final Stylo `Stylist` integration can
//! replace this module without changing `Page::computed_style(node_id)` or the
//! WPT harness seam.

#![forbid(unsafe_code)]

use std::cmp::Ordering;

use crate::doc::Document;
use crate::media_query::Viewport;
use crate::style_dom::ElementRelation;
use crate::style_dom::Selector;

mod conditions;
use conditions::RuleCondition;

const DEFAULT_VIEWPORT: (u32, u32) = (800, 600);

/// Parsed author stylesheet state for a [`Page`](crate::page::Page).
#[derive(Debug, Clone, Default)]
pub struct AuthorStylesheet {
    rules: Vec<StyleRule>,
    layers: Vec<String>,
    next_source_order: u32,
    next_anonymous_layer: u16,
}

impl AuthorStylesheet {
    /// Parse every `<style>` block in document order.
    pub fn from_blocks(blocks: &[String]) -> Self {
        let mut sheet = Self::default();
        for block in blocks {
            sheet.extend_block(block);
        }
        sheet
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    pub fn rule_count(&self) -> usize {
        self.rules.len()
    }

    /// Read-only CSSOM-style view of one parsed style rule. This stays small and
    /// stable so the CSSOM host module can expose `document.styleSheets` /
    /// `CSSRule` smoke coverage without re-walking cascade internals.
    pub fn rule(&self, index: usize) -> Option<AuthorStyleRule<'_>> {
        self.rules.get(index).map(|rule| AuthorStyleRule { rule })
    }

    /// Compute the current cascade projection for one element.
    pub fn computed_style(&self, document: &Document, node_id: usize) -> Vec<(String, String)> {
        self.computed_style_for_viewport(document, node_id, DEFAULT_VIEWPORT)
    }

    /// Compute the cascade projection for one element under a concrete viewport.
    ///
    /// Renderer source projection uses this so `@media` rules respond to the
    /// exact host viewport, while public inspection keeps the 800×600 default.
    pub fn computed_style_for_viewport(
        &self,
        document: &Document,
        node_id: usize,
        viewport: (u32, u32),
    ) -> Vec<(String, String)> {
        self.computed_style_inner(document, node_id, viewport, 0)
    }

    fn computed_style_inner(
        &self,
        document: &Document,
        node_id: usize,
        viewport: (u32, u32),
        depth: usize,
    ) -> Vec<(String, String)> {
        if depth > 64 || document.element_by_node_id(node_id).is_none() {
            return Vec::new();
        }

        let parent_styles = document
            .related_element_by_node_id(node_id, ElementRelation::Parent)
            .map(|parent| self.computed_style_inner(document, parent.node_id, viewport, depth + 1))
            .unwrap_or_default();
        let viewport = Viewport::new(viewport.0 as f64, viewport.1 as f64, 1.0);
        let mut out = CascadedStyle::new();

        for rule in &self.rules {
            if !rule
                .conditions
                .iter()
                .all(|condition| condition.matches(&viewport))
            {
                continue;
            }
            if document.matches_selector(node_id, &rule.selector) {
                for declaration in &rule.declarations {
                    out.apply(
                        declaration,
                        declaration.weight(rule.specificity, Origin::Author, rule.layer),
                    );
                }
            }
        }

        if let Some(element) = document.element_by_node_id(node_id)
            && let Some((_, inline)) = element
                .attributes
                .into_iter()
                .find(|(name, _)| name == "style")
        {
            for declaration in parse_declarations(&inline, self.next_source_order) {
                out.apply(
                    &declaration,
                    declaration.weight(Specificity::INLINE, Origin::Inline, None),
                );
            }
        }

        out.finish(&parent_styles)
    }

    fn extend_block(&mut self, css: &str) {
        let css = strip_comments(css);
        self.extend_block_scoped(&css, None, &[]);
    }

    fn extend_block_scoped(&mut self, css: &str, layer: Option<u16>, conditions: &[RuleCondition]) {
        let mut cursor = 0usize;
        while let Some(open_rel) = find_top_level_char(&css[cursor..], '{') {
            let open = cursor + open_rel;
            let raw_prelude = css[cursor..open].trim();
            let selector_text = self.process_prelude_statements(raw_prelude).trim();
            let Some(close) = find_matching_brace(css, open) else {
                break;
            };
            let body = &css[open + 1..close];
            cursor = close + 1;

            if selector_text.is_empty() || selector_text.starts_with('@') {
                self.extend_at_rule(selector_text, body, layer, conditions);
                continue;
            }

            let declarations = parse_declarations(body, self.next_source_order);
            self.next_source_order += declarations.len() as u32;
            if declarations.is_empty() {
                continue;
            }

            for selector_text in split_top_level(selector_text, ',') {
                let selector_text = selector_text.trim();
                if selector_text.is_empty() {
                    continue;
                }
                if let Ok(selector) = Selector::parse(selector_text) {
                    self.rules.push(StyleRule {
                        selector,
                        selector_text: selector_text.to_owned(),
                        specificity: Specificity::parse(selector_text),
                        layer,
                        conditions: conditions.to_vec(),
                        declarations: declarations.clone(),
                    });
                }
            }
        }
    }

    fn process_prelude_statements<'a>(&mut self, raw: &'a str) -> &'a str {
        let mut parts = split_top_level(raw, ';');
        let prelude = parts.pop().unwrap_or_default();
        for statement in parts {
            self.process_statement(statement.trim());
        }
        prelude
    }

    fn process_statement(&mut self, statement: &str) {
        if let Some(names) = statement.strip_prefix("@layer") {
            for name in split_top_level(names, ',') {
                let name = name.trim();
                if !name.is_empty() {
                    self.ensure_layer(name);
                }
            }
        }
    }

    fn extend_at_rule(
        &mut self,
        prelude: &str,
        body: &str,
        layer: Option<u16>,
        conditions: &[RuleCondition],
    ) {
        let prelude = prelude.trim();
        if let Some(query) = prelude.strip_prefix("@media") {
            let mut next = conditions.to_vec();
            next.push(RuleCondition::media(query.trim()));
            self.extend_block_scoped(body, layer, &next);
        } else if let Some(condition) = prelude.strip_prefix("@supports") {
            let mut next = conditions.to_vec();
            next.push(RuleCondition::supports(condition.trim()));
            self.extend_block_scoped(body, layer, &next);
        } else if let Some(names) = prelude.strip_prefix("@layer") {
            let next_layer = self.layer_for_block(names.trim());
            self.extend_block_scoped(body, next_layer, conditions);
        }
    }

    fn layer_for_block(&mut self, names: &str) -> Option<u16> {
        let name = split_top_level(names, ',')
            .into_iter()
            .map(str::trim)
            .find(|name| !name.is_empty());
        match name {
            Some(name) => Some(self.ensure_layer(name)),
            None => {
                let name = format!("#anonymous-{}", self.next_anonymous_layer);
                self.next_anonymous_layer = self.next_anonymous_layer.saturating_add(1);
                Some(self.ensure_layer(&name))
            }
        }
    }

    fn ensure_layer(&mut self, name: &str) -> u16 {
        if let Some(idx) = self.layers.iter().position(|layer| layer == name) {
            return idx as u16;
        }
        let idx = self.layers.len().min(u16::MAX as usize) as u16;
        self.layers.push(name.to_owned());
        idx
    }
}

pub fn css_supports(input: &str) -> bool {
    conditions::supports_condition(input)
}

/// Borrowed CSSOM-style view of a parsed author rule.
#[derive(Debug, Clone, Copy)]
pub struct AuthorStyleRule<'a> {
    rule: &'a StyleRule,
}

impl<'a> AuthorStyleRule<'a> {
    pub fn selector_text(&self) -> &'a str {
        &self.rule.selector_text
    }

    pub fn declaration_count(&self) -> usize {
        self.rule.declarations.len()
    }

    pub fn declaration_property(&self, index: usize) -> Option<&'a str> {
        self.rule
            .declarations
            .get(index)
            .map(|declaration| declaration.property.as_str())
    }

    pub fn get_property_value(&self, property: &str) -> Option<&'a str> {
        self.rule
            .declarations
            .iter()
            .find(|declaration| declaration.property == property)
            .map(|declaration| declaration.value.as_str())
    }

    pub fn css_text(&self) -> String {
        let declarations = self
            .rule
            .declarations
            .iter()
            .map(|declaration| {
                let important = if declaration.important {
                    " !important"
                } else {
                    ""
                };
                format!(
                    "{}: {}{};",
                    declaration.property, declaration.value, important
                )
            })
            .collect::<Vec<_>>()
            .join(" ");
        format!("{} {{ {} }}", self.selector_text(), declarations)
    }
}

#[derive(Debug, Clone)]
struct StyleRule {
    selector: Selector,
    selector_text: String,
    specificity: Specificity,
    layer: Option<u16>,
    conditions: Vec<RuleCondition>,
    declarations: Vec<Declaration>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declaration {
    pub property: String,
    pub value: String,
    pub important: bool,
    source_order: u32,
}

impl Declaration {
    fn weight(
        &self,
        specificity: Specificity,
        origin: Origin,
        layer: Option<u16>,
    ) -> CascadeWeight {
        CascadeWeight {
            tier: match (origin, self.important) {
                (Origin::Author, false) => 0,
                (Origin::Inline, false) => 1,
                (Origin::Author, true) => 2,
                (Origin::Inline, true) => 3,
            },
            layer_rank: layer_rank(origin, self.important, layer),
            specificity,
            source_order: self.source_order,
        }
    }
}

fn layer_rank(origin: Origin, important: bool, layer: Option<u16>) -> i32 {
    if matches!(origin, Origin::Inline) {
        return i32::MAX;
    }
    match (important, layer) {
        // Normal unlayered author declarations beat normal layered declarations.
        (false, None) => i32::MAX,
        // Later layers beat earlier layers for normal declarations.
        (false, Some(layer)) => i32::from(layer),
        // Important layer order is reversed by CSS Cascade 5.
        (true, Some(layer)) => i32::MAX - i32::from(layer),
        // Important unlayered declarations lose to important layered ones.
        (true, None) => 0,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Origin {
    Author,
    Inline,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
struct Specificity {
    ids: u16,
    classes: u16,
    elements: u16,
}

impl Specificity {
    const INLINE: Self = Self {
        ids: u16::MAX,
        classes: u16::MAX,
        elements: u16::MAX,
    };

    fn parse(selector: &str) -> Self {
        specificity_for_selector(selector, false)
    }

    fn max(self, other: Self) -> Self {
        if other > self { other } else { self }
    }

    fn add(self, other: Self) -> Self {
        Self {
            ids: self.ids.saturating_add(other.ids),
            classes: self.classes.saturating_add(other.classes),
            elements: self.elements.saturating_add(other.elements),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CascadeWeight {
    tier: u8,
    layer_rank: i32,
    specificity: Specificity,
    source_order: u32,
}

impl CascadeWeight {
    fn inherited_custom_property() -> Self {
        Self {
            tier: 0,
            layer_rank: 0,
            specificity: Specificity::default(),
            source_order: 0,
        }
    }
}

impl Ord for CascadeWeight {
    fn cmp(&self, other: &Self) -> Ordering {
        self.tier
            .cmp(&other.tier)
            .then_with(|| self.layer_rank.cmp(&other.layer_rank))
            .then_with(|| self.specificity.cmp(&other.specificity))
            .then_with(|| self.source_order.cmp(&other.source_order))
    }
}

impl PartialOrd for CascadeWeight {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

struct CascadedStyle {
    entries: Vec<CascadedEntry>,
}

impl CascadedStyle {
    fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    fn apply(&mut self, declaration: &Declaration, weight: CascadeWeight) {
        if let Some(entry) = self
            .entries
            .iter_mut()
            .find(|entry| entry.property == declaration.property)
        {
            if weight >= entry.weight {
                entry.value = declaration.value.clone();
                entry.weight = weight;
            }
        } else {
            self.entries.push(CascadedEntry {
                property: declaration.property.clone(),
                value: declaration.value.clone(),
                weight,
            });
        }
    }

    fn finish(self, parent_styles: &[(String, String)]) -> Vec<(String, String)> {
        let mut entries = apply_css_wide_keywords(self.entries, parent_styles);
        for (property, value) in parent_styles
            .iter()
            .filter(|(property, _)| property.starts_with("--"))
        {
            if entries.iter().all(|entry| entry.property != *property) {
                entries.push(CascadedEntry {
                    property: property.clone(),
                    value: value.clone(),
                    weight: CascadeWeight::inherited_custom_property(),
                });
            }
        }

        let custom_properties: Vec<(String, String)> = entries
            .iter()
            .filter(|entry| entry.property.starts_with("--"))
            .map(|entry| (entry.property.clone(), entry.value.clone()))
            .collect();

        entries
            .into_iter()
            .filter_map(|entry| {
                if entry.property.starts_with("--") {
                    return Some((entry.property, entry.value));
                }
                resolve_var_functions(&entry.value, &custom_properties, 0)
                    .map(|value| (entry.property, value))
            })
            .collect()
    }
}

struct CascadedEntry {
    property: String,
    value: String,
    weight: CascadeWeight,
}

fn apply_css_wide_keywords(
    entries: Vec<CascadedEntry>,
    parent_styles: &[(String, String)],
) -> Vec<CascadedEntry> {
    entries
        .into_iter()
        .filter_map(|mut entry| {
            let keyword = entry.value.trim().to_ascii_lowercase();
            match keyword.as_str() {
                "inherit" => {
                    entry.value = parent_style_value(parent_styles, &entry.property)?.to_owned();
                    Some(entry)
                }
                "unset"
                    if entry.property.starts_with("--")
                        || is_inherited_property(&entry.property) =>
                {
                    entry.value = parent_style_value(parent_styles, &entry.property)?.to_owned();
                    Some(entry)
                }
                "unset" | "initial" | "revert" | "revert-layer" => None,
                _ => Some(entry),
            }
        })
        .collect()
}

fn parent_style_value<'a>(styles: &'a [(String, String)], property: &str) -> Option<&'a str> {
    styles
        .iter()
        .find(|(name, _)| name == property)
        .map(|(_, value)| value.as_str())
}

fn is_inherited_property(property: &str) -> bool {
    matches!(
        property,
        "color"
            | "font"
            | "font-family"
            | "font-size"
            | "font-style"
            | "font-weight"
            | "line-height"
            | "letter-spacing"
            | "text-align"
            | "text-indent"
            | "text-transform"
            | "visibility"
            | "white-space"
            | "word-spacing"
    )
}

fn resolve_var_functions(
    value: &str,
    custom_properties: &[(String, String)],
    depth: usize,
) -> Option<String> {
    if depth > 16 {
        return None;
    }
    let Some(start) = value.find("var(") else {
        return Some(value.to_owned());
    };
    let open = start + "var".len();
    let close = find_matching_paren(value, open)?;
    let args = &value[open + 1..close];
    let pieces = split_top_level(args, ',');
    let name = pieces.first()?.trim();
    if !name.starts_with("--") || name.len() <= 2 {
        return None;
    }
    let replacement = custom_properties
        .iter()
        .rev()
        .find(|(property, _)| property == name)
        .and_then(|(_, value)| resolve_var_functions(value, custom_properties, depth + 1))
        .or_else(|| {
            if pieces.len() > 1 {
                let fallback_start = args.find(',')? + 1;
                resolve_var_functions(args[fallback_start..].trim(), custom_properties, depth + 1)
            } else {
                None
            }
        })?;

    let mut out = String::with_capacity(value.len() + replacement.len());
    out.push_str(&value[..start]);
    out.push_str(&replacement);
    out.push_str(&value[close + 1..]);
    resolve_var_functions(&out, custom_properties, depth + 1)
}

fn find_matching_paren(input: &str, open: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (idx, ch) in input[open..].char_indices() {
        let idx = open + idx;
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == q {
                quote = None;
            }
            continue;
        }
        match ch {
            '\'' | '"' => quote = Some(ch),
            '(' => depth += 1,
            ')' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(idx);
                }
            }
            _ => {}
        }
    }
    None
}

/// Parse a CSS declaration list (`name: value; ...`). Also used for inline
/// `style` attributes.
pub fn parse_declarations(css: &str, base_source_order: u32) -> Vec<Declaration> {
    let mut out: Vec<Declaration> = Vec::new();
    for (idx, declaration) in split_top_level(css, ';').into_iter().enumerate() {
        let Some((name, value)) = split_once_top_level(declaration, ':') else {
            continue;
        };
        let Some(property) = normalise_property_name(name) else {
            continue;
        };
        let Some((value, important)) = normalise_declaration_value(value) else {
            continue;
        };
        out.push(Declaration {
            property,
            value,
            important,
            source_order: base_source_order + idx as u32,
        });
    }
    out
}

fn normalise_property_name(name: &str) -> Option<String> {
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    if name.starts_with("--") {
        return Some(name.to_owned());
    }
    let lower = name.to_ascii_lowercase();
    if lower
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        Some(lower)
    } else {
        None
    }
}

fn normalise_declaration_value(value: &str) -> Option<(String, bool)> {
    let (value, important) = strip_important(value.trim());
    let value = value.trim();
    if value.is_empty() {
        None
    } else {
        Some((value.to_owned(), important))
    }
}

fn strip_important(value: &str) -> (&str, bool) {
    let mut last_bang = None;
    for (idx, ch) in top_level_chars(value) {
        if ch == '!' {
            last_bang = Some(idx);
        }
    }
    let Some(idx) = last_bang else {
        return (value, false);
    };
    let suffix = value[idx + 1..].trim();
    if suffix.eq_ignore_ascii_case("important") {
        (&value[..idx], true)
    } else {
        (value, false)
    }
}

fn strip_comments(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.char_indices().peekable();
    let mut quote: Option<char> = None;
    let mut escaped = false;

    while let Some((idx, ch)) = chars.next() {
        if let Some(q) = quote {
            out.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == q {
                quote = None;
            }
            continue;
        }

        if ch == '\'' || ch == '"' {
            quote = Some(ch);
            out.push(ch);
            continue;
        }

        if ch == '/'
            && let Some(&(_, '*')) = chars.peek()
        {
            let _ = chars.next();
            let mut prev = '\0';
            for (_, c) in chars.by_ref() {
                if prev == '*' && c == '/' {
                    break;
                }
                prev = c;
            }
            out.push(' ');
            continue;
        }

        out.push_str(&input[idx..idx + ch.len_utf8()]);
    }
    out
}

fn find_top_level_char(input: &str, target: char) -> Option<usize> {
    top_level_chars(input)
        .find(|(_, ch)| *ch == target)
        .map(|(idx, _)| idx)
}

fn find_matching_brace(input: &str, open: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (idx, ch) in input[open..].char_indices() {
        let idx = open + idx;
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == q {
                quote = None;
            }
            continue;
        }
        match ch {
            '\'' | '"' => quote = Some(ch),
            '{' => depth += 1,
            '}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(idx);
                }
            }
            _ => {}
        }
    }
    None
}

fn split_top_level(input: &str, delimiter: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0usize;
    for (idx, ch) in top_level_chars(input) {
        if ch == delimiter {
            parts.push(&input[start..idx]);
            start = idx + ch.len_utf8();
        }
    }
    parts.push(&input[start..]);
    parts
}

fn split_once_top_level(input: &str, delimiter: char) -> Option<(&str, &str)> {
    for (idx, ch) in top_level_chars(input) {
        if ch == delimiter {
            let rhs = idx + ch.len_utf8();
            return Some((&input[..idx], &input[rhs..]));
        }
    }
    None
}

fn top_level_chars(input: &str) -> impl Iterator<Item = (usize, char)> + '_ {
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    input.char_indices().filter_map(move |(idx, ch)| {
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == q {
                quote = None;
            }
            return None;
        }

        match ch {
            '\'' | '"' => {
                quote = Some(ch);
                None
            }
            '(' | '[' => {
                depth = depth.saturating_add(1);
                None
            }
            ')' | ']' => {
                depth = depth.saturating_sub(1);
                None
            }
            _ if depth == 0 => Some((idx, ch)),
            _ => None,
        }
    })
}

fn specificity_for_selector(selector: &str, zeroed: bool) -> Specificity {
    let mut out = Specificity {
        ids: 0,
        classes: 0,
        elements: 0,
    };
    let bytes = selector.as_bytes();
    let mut i = 0usize;
    let mut may_start_type = true;
    while i < bytes.len() {
        let ch = selector[i..].chars().next().unwrap();
        match ch {
            '#' => {
                out.ids = out.ids.saturating_add(u16::from(!zeroed));
                i = skip_ident(selector, i + 1);
                may_start_type = false;
            }
            '.' => {
                out.classes = out.classes.saturating_add(u16::from(!zeroed));
                i = skip_ident(selector, i + 1);
                may_start_type = false;
            }
            '[' => {
                out.classes = out.classes.saturating_add(u16::from(!zeroed));
                i = skip_balanced(selector, i, '[', ']');
                may_start_type = false;
            }
            ':' => {
                if selector[i + 1..].starts_with(':') {
                    out.elements = out.elements.saturating_add(u16::from(!zeroed));
                    i = skip_ident(selector, i + 2);
                } else {
                    let name_start = i + 1;
                    let name_end = skip_ident(selector, name_start);
                    let name = &selector[name_start..name_end];
                    if selector[name_end..].starts_with('(') {
                        let close = skip_balanced(selector, name_end, '(', ')');
                        let args = &selector[name_end + 1..close.saturating_sub(1)];
                        if name.eq_ignore_ascii_case("where") {
                            // `:where()` contributes zero specificity.
                        } else if matches!(name, "is" | "not" | "has") {
                            let max = split_top_level(args, ',')
                                .into_iter()
                                .map(|arg| specificity_for_selector(arg, zeroed))
                                .fold(Specificity::default(), Specificity::max);
                            out = out.add(max);
                        } else {
                            out.classes = out.classes.saturating_add(u16::from(!zeroed));
                        }
                        i = close;
                    } else {
                        out.classes = out.classes.saturating_add(u16::from(!zeroed));
                        i = name_end;
                    }
                }
                may_start_type = false;
            }
            '*' => {
                i += ch.len_utf8();
                may_start_type = false;
            }
            '>' | '+' | '~' | ',' => {
                i += ch.len_utf8();
                may_start_type = true;
            }
            c if c.is_whitespace() => {
                i += ch.len_utf8();
                may_start_type = true;
            }
            c if may_start_type && is_ident_start(c) => {
                out.elements = out.elements.saturating_add(u16::from(!zeroed));
                i = skip_ident(selector, i);
                may_start_type = false;
            }
            _ => {
                i += ch.len_utf8();
            }
        }
    }
    out
}

fn skip_ident(input: &str, mut idx: usize) -> usize {
    while idx < input.len() {
        let ch = input[idx..].chars().next().unwrap();
        if is_ident_continue(ch) || ch == '-' {
            idx += ch.len_utf8();
        } else {
            break;
        }
    }
    idx
}

fn is_ident_start(ch: char) -> bool {
    ch == '_' || ch == '-' || ch.is_ascii_alphabetic() || !ch.is_ascii()
}

fn is_ident_continue(ch: char) -> bool {
    is_ident_start(ch) || ch.is_ascii_digit()
}

fn skip_balanced(input: &str, open: usize, open_ch: char, close_ch: char) -> usize {
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (rel, ch) in input[open..].char_indices() {
        let idx = open + rel;
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == q {
                quote = None;
            }
            continue;
        }
        match ch {
            '\'' | '"' => quote = Some(ch),
            c if c == open_ch => depth += 1,
            c if c == close_ch => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return idx + ch.len_utf8();
                }
            }
            _ => {}
        }
    }
    input.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(html: &str) -> Document {
        Document::parse(html).unwrap()
    }

    #[test]
    fn declarations_parse_important_and_nested_delimiters() {
        let declarations = parse_declarations(
            "Color: red; background-image: url('a;b:c'); color: blue ! important; --Token: A:B",
            10,
        );
        assert_eq!(declarations.len(), 4);
        assert_eq!(declarations[0].property, "color");
        assert_eq!(declarations[0].value, "red");
        assert!(!declarations[0].important);
        assert_eq!(declarations[1].value, "url('a;b:c')");
        assert_eq!(declarations[2].property, "color");
        assert_eq!(declarations[2].value, "blue");
        assert!(declarations[2].important);
        assert_eq!(declarations[3].property, "--Token");
        assert_eq!(declarations[3].source_order, 13);
    }

    #[test]
    fn declarations_ignore_bad_names_and_empty_values() {
        let declarations = parse_declarations("; : red; bad$name: nope; color: ; ok-name: yes", 0);
        assert_eq!(declarations.len(), 1);
        assert_eq!(declarations[0].property, "ok-name");
    }

    #[test]
    fn stylesheet_parses_rules_and_selector_lists() {
        let sheet = AuthorStylesheet::from_blocks(&[String::from(
            "/* comment */ #a, .b { color: red; display: block } @media screen { p { color: blue } }",
        )]);
        assert_eq!(sheet.rules.len(), 3);
        assert_eq!(sheet.rules[0].specificity, sp(1, 0, 0));
        assert_eq!(sheet.rules[1].specificity, sp(0, 1, 0));
        assert_eq!(sheet.rules[2].specificity, sp(0, 0, 1));
    }

    #[test]
    fn stylesheet_exposes_cssom_rule_view() {
        let sheet = AuthorStylesheet::from_blocks(&[String::from(
            "#a { color: red; display: block !important } .b { color: blue }",
        )]);
        let rule = sheet.rule(0).unwrap();
        assert_eq!(rule.selector_text(), "#a");
        assert_eq!(rule.declaration_count(), 2);
        assert_eq!(rule.declaration_property(0), Some("color"));
        assert_eq!(rule.get_property_value("display"), Some("block"));
        assert_eq!(
            rule.css_text(),
            "#a { color: red; display: block !important; }"
        );
        assert!(sheet.rule(2).is_none());
    }

    #[test]
    fn cascade_applies_matching_stylesheet_rules() {
        let doc = doc(
            "<html><head><style>p { color: red; display: block } .lead { color: blue }</style></head><body><p id='x' class='lead'>x</p></body></html>",
        );
        let sheet = AuthorStylesheet::from_blocks(&doc.style_blocks());
        let node_id = doc
            .query_first(&Selector::parse("#x").unwrap())
            .unwrap()
            .node_id;
        let styles = sheet.computed_style(&doc, node_id);
        assert_eq!(value(&styles, "display"), Some("block"));
        assert_eq!(value(&styles, "color"), Some("blue"));
    }

    #[test]
    fn cascade_uses_specificity_over_source_order() {
        let doc = doc("<style>#x { color: red } p { color: green }</style><p id='x'>x</p>");
        let sheet = AuthorStylesheet::from_blocks(&doc.style_blocks());
        let node_id = doc
            .query_first(&Selector::parse("#x").unwrap())
            .unwrap()
            .node_id;
        assert_eq!(
            value(&sheet.computed_style(&doc, node_id), "color"),
            Some("red")
        );
    }

    #[test]
    fn cascade_uses_source_order_for_equal_specificity() {
        let document =
            doc("<style>.a { color: red } .b { color: green }</style><p id='x' class='a b'>x</p>");
        let sheet = AuthorStylesheet::from_blocks(&document.style_blocks());
        let node_id = document
            .query_first(&Selector::parse("#x").unwrap())
            .unwrap()
            .node_id;
        assert_eq!(
            value(&sheet.computed_style(&document, node_id), "color"),
            Some("green")
        );
    }

    #[test]
    fn important_author_rule_beats_inline_normal_but_not_inline_important() {
        let document =
            doc("<style>#x { color: red !important }</style><p id='x' style='color: green'>x</p>");
        let sheet = AuthorStylesheet::from_blocks(&document.style_blocks());
        let node_id = document
            .query_first(&Selector::parse("#x").unwrap())
            .unwrap()
            .node_id;
        assert_eq!(
            value(&sheet.computed_style(&document, node_id), "color"),
            Some("red")
        );

        let document = doc(
            "<style>#x { color: red !important }</style><p id='x' style='color: green !important'>x</p>",
        );
        let sheet = AuthorStylesheet::from_blocks(&document.style_blocks());
        let node_id = document
            .query_first(&Selector::parse("#x").unwrap())
            .unwrap()
            .node_id;
        assert_eq!(
            value(&sheet.computed_style(&document, node_id), "color"),
            Some("green")
        );
    }

    #[test]
    fn media_queries_gate_rules_by_viewport() {
        let document = doc(
            "<style>p { color: red } @media (max-width: 400px) { p { color: blue } }</style><p id='x'>x</p>",
        );
        let sheet = AuthorStylesheet::from_blocks(&document.style_blocks());
        let node_id = document
            .query_first(&Selector::parse("#x").unwrap())
            .unwrap()
            .node_id;

        assert_eq!(
            value(
                &sheet.computed_style_for_viewport(&document, node_id, (800, 600)),
                "color"
            ),
            Some("red")
        );
        assert_eq!(
            value(
                &sheet.computed_style_for_viewport(&document, node_id, (360, 600)),
                "color"
            ),
            Some("blue")
        );
    }

    #[test]
    fn supports_conditions_gate_rules_fail_closed() {
        let document = doc(
            "<style>@supports (display: grid) { #x { display: grid } } @supports (unknown-prop: yes) { #x { color: red } }</style><p id='x'>x</p>",
        );
        let sheet = AuthorStylesheet::from_blocks(&document.style_blocks());
        let node_id = document
            .query_first(&Selector::parse("#x").unwrap())
            .unwrap()
            .node_id;
        let styles = sheet.computed_style(&document, node_id);
        assert_eq!(value(&styles, "display"), Some("grid"));
        assert_eq!(value(&styles, "color"), None);
    }

    #[test]
    fn cascade_layers_apply_normal_and_important_ordering() {
        let document = doc(
            "<style>@layer base, theme; @layer base { #x { color: red } } @layer theme { #x { color: blue } } #x { display: block }</style><p id='x'>x</p>",
        );
        let sheet = AuthorStylesheet::from_blocks(&document.style_blocks());
        let node_id = document
            .query_first(&Selector::parse("#x").unwrap())
            .unwrap()
            .node_id;
        let styles = sheet.computed_style(&document, node_id);
        assert_eq!(value(&styles, "color"), Some("blue"));
        assert_eq!(value(&styles, "display"), Some("block"));

        let document = doc(
            "<style>@layer base, theme; @layer base { #x { color: red !important } } @layer theme { #x { color: blue !important } } #x { color: green !important }</style><p id='x'>x</p>",
        );
        let sheet = AuthorStylesheet::from_blocks(&document.style_blocks());
        let node_id = document
            .query_first(&Selector::parse("#x").unwrap())
            .unwrap()
            .node_id;
        assert_eq!(
            value(&sheet.computed_style(&document, node_id), "color"),
            Some("red")
        );
    }

    #[test]
    fn custom_properties_and_css_wide_keywords_resolve() {
        let document = doc(
            "<style>#parent { color: purple; --accent: blue } #child { color: inherit; background-color: var(--accent, red); display: initial }</style><div id='parent'><p id='child'>x</p></div>",
        );
        let sheet = AuthorStylesheet::from_blocks(&document.style_blocks());
        let child = document
            .query_first(&Selector::parse("#child").unwrap())
            .unwrap()
            .node_id;
        let styles = sheet.computed_style(&document, child);
        assert_eq!(value(&styles, "color"), Some("purple"));
        assert_eq!(value(&styles, "background-color"), Some("blue"));
        assert_eq!(value(&styles, "display"), None);
    }

    #[test]
    fn specificity_counts_common_selector_shapes() {
        assert_eq!(Specificity::parse("p"), sp(0, 0, 1));
        assert_eq!(Specificity::parse("#id .class[attr] p:hover"), sp(1, 3, 1));
        assert_eq!(Specificity::parse(":where(#id) p"), sp(0, 0, 1));
        assert_eq!(Specificity::parse(":is(.a, #b) p"), sp(1, 0, 1));
        assert_eq!(Specificity::parse("#x:is(.a, .b) p"), sp(1, 1, 1));
        assert_eq!(Specificity::parse("p::before"), sp(0, 0, 2));
    }

    fn sp(ids: u16, classes: u16, elements: u16) -> Specificity {
        Specificity {
            ids,
            classes,
            elements,
        }
    }

    fn value<'a>(styles: &'a [(String, String)], property: &str) -> Option<&'a str> {
        styles
            .iter()
            .find(|(name, _)| name == property)
            .map(|(_, value)| value.as_str())
    }
}
