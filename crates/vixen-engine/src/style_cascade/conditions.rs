use crate::media_query::{MediaQuery, Viewport};

use super::{
    find_matching_paren, normalise_declaration_value, normalise_property_name,
    split_once_top_level, split_top_level,
};

#[derive(Debug, Clone)]
pub(super) enum RuleCondition {
    Media(MediaConditionList),
    Supports(SupportsCondition),
}

impl RuleCondition {
    pub(super) fn media(input: &str) -> Self {
        Self::Media(MediaConditionList::parse(input))
    }

    pub(super) fn supports(input: &str) -> Self {
        Self::Supports(SupportsCondition::parse(input))
    }

    pub(super) fn matches(&self, viewport: &Viewport) -> bool {
        match self {
            RuleCondition::Media(condition) => condition.matches(viewport),
            RuleCondition::Supports(condition) => condition.matches(),
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct MediaConditionList {
    queries: Vec<MediaQuery>,
}

impl MediaConditionList {
    fn parse(input: &str) -> Self {
        let queries = split_top_level(input, ',')
            .into_iter()
            .filter_map(|query| MediaQuery::parse(query.trim()).ok())
            .collect();
        Self { queries }
    }

    fn matches(&self, viewport: &Viewport) -> bool {
        !self.queries.is_empty() && self.queries.iter().any(|query| query.matches(viewport))
    }
}

#[derive(Debug, Clone)]
pub(super) enum SupportsCondition {
    Declaration(String, String),
    Not(Box<SupportsCondition>),
    And(Vec<SupportsCondition>),
    Or(Vec<SupportsCondition>),
    Invalid,
}

impl SupportsCondition {
    fn parse(input: &str) -> Self {
        parse_supports_condition(input.trim())
    }

    fn matches(&self) -> bool {
        match self {
            SupportsCondition::Declaration(property, value) => {
                supports_declaration(property, value)
            }
            SupportsCondition::Not(inner) => !inner.matches(),
            SupportsCondition::And(items) => !items.is_empty() && items.iter().all(Self::matches),
            SupportsCondition::Or(items) => !items.is_empty() && items.iter().any(Self::matches),
            SupportsCondition::Invalid => false,
        }
    }
}

fn parse_supports_condition(input: &str) -> SupportsCondition {
    let input = input.trim();
    if input.is_empty() {
        return SupportsCondition::Invalid;
    }
    if let Some(rest) = input.strip_prefix("not ") {
        return SupportsCondition::Not(Box::new(parse_supports_condition(rest)));
    }
    let or_parts = split_top_level_keyword(input, "or");
    if or_parts.len() > 1 {
        return SupportsCondition::Or(or_parts.into_iter().map(parse_supports_condition).collect());
    }
    let and_parts = split_top_level_keyword(input, "and");
    if and_parts.len() > 1 {
        return SupportsCondition::And(
            and_parts
                .into_iter()
                .map(parse_supports_condition)
                .collect(),
        );
    }
    if let Some(inner) = strip_wrapping_parens(input) {
        if let Some((property, value)) = split_once_top_level(inner, ':') {
            let Some(property) = normalise_property_name(property) else {
                return SupportsCondition::Invalid;
            };
            let Some((value, _)) = normalise_declaration_value(value) else {
                return SupportsCondition::Invalid;
            };
            return SupportsCondition::Declaration(property, value);
        }
        return parse_supports_condition(inner);
    }
    SupportsCondition::Invalid
}

fn supports_declaration(property: &str, _value: &str) -> bool {
    property.starts_with("--")
        || matches!(
            property,
            "background"
                | "background-color"
                | "background-image"
                | "border"
                | "border-width"
                | "border-top-width"
                | "border-right-width"
                | "border-bottom-width"
                | "border-left-width"
                | "box-sizing"
                | "color"
                | "column-gap"
                | "display"
                | "flex-basis"
                | "flex-direction"
                | "flex-grow"
                | "flex-shrink"
                | "font-size"
                | "gap"
                | "grid-template-columns"
                | "grid-template-rows"
                | "height"
                | "left"
                | "margin"
                | "margin-top"
                | "margin-right"
                | "margin-bottom"
                | "margin-left"
                | "opacity"
                | "overflow"
                | "overflow-x"
                | "overflow-y"
                | "padding"
                | "padding-top"
                | "padding-right"
                | "padding-bottom"
                | "padding-left"
                | "position"
                | "right"
                | "row-gap"
                | "top"
                | "width"
                | "z-index"
        )
}

fn split_top_level_keyword<'a>(input: &'a str, keyword: &str) -> Vec<&'a str> {
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut idx = 0usize;
    while idx < input.len() {
        let ch = input[idx..].chars().next().unwrap();
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == q {
                quote = None;
            }
            idx += ch.len_utf8();
            continue;
        }
        match ch {
            '\'' | '"' => quote = Some(ch),
            '(' | '[' => depth = depth.saturating_add(1),
            ')' | ']' => depth = depth.saturating_sub(1),
            _ if depth == 0
                && input[idx..].starts_with(keyword)
                && is_keyword_boundary(input, idx, keyword.len()) =>
            {
                parts.push(input[start..idx].trim());
                idx += keyword.len();
                start = idx;
                continue;
            }
            _ => {}
        }
        idx += ch.len_utf8();
    }
    parts.push(input[start..].trim());
    parts
}

fn is_keyword_boundary(input: &str, start: usize, len: usize) -> bool {
    let before = input[..start].chars().next_back();
    let after = input[start + len..].chars().next();
    before.is_none_or(|ch| ch.is_whitespace() || ch == ')')
        && after.is_none_or(|ch| ch.is_whitespace() || ch == '(')
}

fn strip_wrapping_parens(input: &str) -> Option<&str> {
    let input = input.trim();
    if !input.starts_with('(') || !input.ends_with(')') {
        return None;
    }
    let close = find_matching_paren(input, 0)?;
    if close == input.len() - 1 {
        Some(input[1..close].trim())
    } else {
        None
    }
}
