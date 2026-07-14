use crate::box_model::{AutoEdges, BoxSizing, Edges, LengthOrAuto};
use crate::color::ColorOrKeyword;
use crate::display_list::Color;
use crate::flex_resolve::FlexDirection;
use crate::grid_resolve::GridTrack;

use super::{LayoutDisplay, LayoutOverflow, LayoutPosition, LayoutStyle};

impl LayoutStyle {
    fn for_tag(tag: &str, inherited_color: Color) -> Self {
        let mut out = Self {
            color: inherited_color,
            ..Self::default()
        };
        if tag == "body" {
            // HTML's default rendering has an 8px body margin in the UA sheet.
            // This keeps the browser-visible origin at the body edge while
            // still letting author CSS (`body { margin: 0 }`) override it.
            out.margin = AutoEdges::px_all(8.0);
        }
        out
    }

    pub(super) fn from_computed(
        tag: &str,
        styles: &[(String, String)],
        inherited_color: Color,
    ) -> Self {
        let mut out = Self::for_tag(tag, inherited_color);

        if style_value(styles, "box-sizing")
            .is_some_and(|value| value.trim().eq_ignore_ascii_case("border-box"))
        {
            out.box_sizing = BoxSizing::BorderBox;
        }
        if let Some(value) = style_value(styles, "width").and_then(parse_auto_length) {
            out.width = value;
        }
        if let Some(value) = style_value(styles, "height").and_then(parse_auto_length) {
            out.height = value;
        }
        if let Some(direction) =
            style_value(styles, "flex-direction").and_then(parse_flex_direction)
        {
            out.flex_direction = direction;
        }
        if let Some(grow) = style_value(styles, "flex-grow").and_then(parse_non_negative_number) {
            out.flex_grow = grow;
        }
        if let Some(shrink) = style_value(styles, "flex-shrink").and_then(parse_non_negative_number)
        {
            out.flex_shrink = shrink;
        }
        if let Some(basis) = style_value(styles, "flex-basis").and_then(parse_auto_length) {
            out.flex_basis = basis;
        }
        if let Some((row_gap, column_gap)) = style_value(styles, "gap").and_then(parse_gap) {
            out.row_gap = row_gap;
            out.column_gap = column_gap;
        }
        if let Some(row_gap) = style_value(styles, "row-gap").and_then(parse_non_negative_length) {
            out.row_gap = row_gap;
        }
        if let Some(column_gap) =
            style_value(styles, "column-gap").and_then(parse_non_negative_length)
        {
            out.column_gap = column_gap;
        }
        if let Some(position) = style_value(styles, "position").and_then(parse_position) {
            out.position = position;
        }
        if let Some(overflow) = style_value(styles, "overflow").and_then(parse_overflow) {
            out.overflow = overflow;
        }
        if let Some(overflow) = style_value(styles, "overflow-x").and_then(parse_overflow) {
            out.overflow = merge_overflow(out.overflow, overflow);
        }
        if let Some(overflow) = style_value(styles, "overflow-y").and_then(parse_overflow) {
            out.overflow = merge_overflow(out.overflow, overflow);
        }
        out.inset.top = style_value(styles, "top").and_then(parse_inset);
        out.inset.right = style_value(styles, "right").and_then(parse_inset);
        out.inset.bottom = style_value(styles, "bottom").and_then(parse_inset);
        out.inset.left = style_value(styles, "left").and_then(parse_inset);
        if let Some(color) = style_value(styles, "color")
            .and_then(|value| parse_color_or_currentcolor(value, out.color))
        {
            out.color = color;
        }
        if let Some(color) = style_value(styles, "background-color")
            .and_then(|value| parse_color_or_currentcolor(value, out.color))
        {
            out.background_color = Some(color);
        }
        if let Some(edges) = style_value(styles, "margin").and_then(parse_auto_edges) {
            out.margin = edges;
        }
        apply_auto_edge(styles, "margin-top", |value| out.margin.top = value);
        apply_auto_edge(styles, "margin-right", |value| out.margin.right = value);
        apply_auto_edge(styles, "margin-bottom", |value| out.margin.bottom = value);
        apply_auto_edge(styles, "margin-left", |value| out.margin.left = value);

        if let Some(edges) = style_value(styles, "padding").and_then(parse_edges) {
            out.padding = edges;
        }
        apply_edge(styles, "padding-top", |value| out.padding.top = value);
        apply_edge(styles, "padding-right", |value| out.padding.right = value);
        apply_edge(styles, "padding-bottom", |value| out.padding.bottom = value);
        apply_edge(styles, "padding-left", |value| out.padding.left = value);

        if let Some(edges) = style_value(styles, "border-width").and_then(parse_border_edges) {
            out.border = edges;
        }
        if let Some(width) = style_value(styles, "border").and_then(parse_border_width) {
            out.border = Edges {
                top: width,
                right: width,
                bottom: width,
                left: width,
            };
        }
        apply_border_edge(styles, "border-top-width", |value| out.border.top = value);
        apply_border_edge(styles, "border-right-width", |value| {
            out.border.right = value
        });
        apply_border_edge(styles, "border-bottom-width", |value| {
            out.border.bottom = value
        });
        apply_border_edge(styles, "border-left-width", |value| out.border.left = value);

        out
    }
}

pub(super) fn display_for(tag: &str, styles: &[(String, String)]) -> Option<LayoutDisplay> {
    if let Some((_, value)) = styles.iter().find(|(property, _)| property == "display") {
        let value = value.trim().to_ascii_lowercase();
        if value == "none" {
            return None;
        }
        if value == "flex" || value == "inline-flex" {
            return Some(LayoutDisplay::Flex);
        }
        if value == "grid" || value == "inline-grid" {
            return Some(LayoutDisplay::Grid);
        }
        if value.starts_with("inline") {
            return Some(LayoutDisplay::Inline);
        }
        return Some(LayoutDisplay::Block);
    }
    Some(default_display_for(tag))
}

fn default_display_for(tag: &str) -> LayoutDisplay {
    match tag {
        "a" | "abbr" | "b" | "bdi" | "bdo" | "br" | "button" | "cite" | "code" | "em" | "i"
        | "img" | "input" | "label" | "mark" | "small" | "span" | "strong" | "sub" | "sup"
        | "textarea" | "time" => LayoutDisplay::Inline,
        _ => LayoutDisplay::Block,
    }
}

pub(super) fn non_rendered_tag(tag: &str) -> bool {
    matches!(
        tag,
        "head" | "title" | "meta" | "link" | "style" | "script" | "noscript" | "template"
    )
}

pub(super) fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn style_value<'a>(styles: &'a [(String, String)], property: &str) -> Option<&'a str> {
    styles
        .iter()
        .find(|(name, _)| name == property)
        .map(|(_, value)| value.as_str())
}

pub(super) fn parse_grid_template(styles: &[(String, String)], property: &str) -> Vec<GridTrack> {
    style_value(styles, property)
        .map(|value| {
            value
                .split_whitespace()
                .filter_map(parse_grid_track)
                .collect()
        })
        .unwrap_or_default()
}

fn parse_grid_track(value: &str) -> Option<GridTrack> {
    let value = value.trim();
    if value.eq_ignore_ascii_case("none") || value.eq_ignore_ascii_case("auto") {
        return None;
    }
    if let Some(length) = parse_non_negative_length(value) {
        return Some(GridTrack::length(length));
    }
    if let Some(fr) = parse_fr_unit(value) {
        return Some(GridTrack::fr(fr));
    }
    parse_minmax_track(value)
}

fn parse_fr_unit(value: &str) -> Option<f32> {
    let number = value
        .trim()
        .strip_suffix("fr")
        .or_else(|| value.trim().strip_suffix("FR"))?;
    if number.is_empty() {
        return Some(1.0);
    }
    parse_non_negative_number(number).filter(|value| *value > 0.0)
}

fn parse_minmax_track(value: &str) -> Option<GridTrack> {
    let inner = value
        .trim()
        .strip_prefix("minmax(")
        .and_then(|value| value.strip_suffix(')'))?;
    let (min, max) = inner.split_once(',')?;
    let min = parse_non_negative_length(min.trim()).unwrap_or(0.0);
    if let Some(fr) = parse_fr_unit(max.trim()) {
        Some(GridTrack::minmax(min, f32::INFINITY, fr))
    } else {
        parse_non_negative_length(max.trim()).map(|max| GridTrack::minmax(min, max, 0.0))
    }
}

fn apply_auto_edge<F>(styles: &[(String, String)], property: &str, apply: F)
where
    F: FnOnce(LengthOrAuto),
{
    if let Some(value) = style_value(styles, property).and_then(parse_auto_length) {
        apply(value);
    }
}

fn apply_edge<F>(styles: &[(String, String)], property: &str, apply: F)
where
    F: FnOnce(f32),
{
    if let Some(value) = style_value(styles, property).and_then(parse_non_negative_length) {
        apply(value);
    }
}

fn apply_border_edge<F>(styles: &[(String, String)], property: &str, apply: F)
where
    F: FnOnce(f32),
{
    if let Some(value) = style_value(styles, property).and_then(parse_border_width) {
        apply(value);
    }
}

fn parse_auto_edges(value: &str) -> Option<AutoEdges> {
    let values = parse_box_shorthand(value, parse_auto_length)?;
    Some(AutoEdges {
        top: values[0],
        right: values[1],
        bottom: values[2],
        left: values[3],
    })
}

fn parse_edges(value: &str) -> Option<Edges> {
    let values = parse_box_shorthand(value, parse_non_negative_length)?;
    Some(Edges {
        top: values[0],
        right: values[1],
        bottom: values[2],
        left: values[3],
    })
}

fn parse_border_edges(value: &str) -> Option<Edges> {
    let values = parse_box_shorthand(value, parse_border_width)?;
    Some(Edges {
        top: values[0],
        right: values[1],
        bottom: values[2],
        left: values[3],
    })
}

fn parse_box_shorthand<T, F>(value: &str, parse_one: F) -> Option<[T; 4]>
where
    T: Copy,
    F: FnMut(&str) -> Option<T>,
{
    let parsed: Vec<T> = value
        .split_whitespace()
        .map(parse_one)
        .collect::<Option<_>>()?;
    match parsed.as_slice() {
        [one] => Some([*one, *one, *one, *one]),
        [block, inline] => Some([*block, *inline, *block, *inline]),
        [top, inline, bottom] => Some([*top, *inline, *bottom, *inline]),
        [top, right, bottom, left] => Some([*top, *right, *bottom, *left]),
        _ => None,
    }
}

fn parse_auto_length(value: &str) -> Option<LengthOrAuto> {
    if value.trim().eq_ignore_ascii_case("auto") {
        return Some(LengthOrAuto::Auto);
    }
    parse_length(value).map(LengthOrAuto::Px)
}

fn parse_non_negative_length(value: &str) -> Option<f32> {
    parse_length(value).map(|value| value.max(0.0))
}

fn parse_non_negative_number(value: &str) -> Option<f32> {
    value
        .trim()
        .parse::<f32>()
        .ok()
        .filter(|value| value.is_finite())
        .map(|value| value.max(0.0))
}

fn parse_flex_direction(value: &str) -> Option<FlexDirection> {
    match value.trim().to_ascii_lowercase().as_str() {
        "row" => Some(FlexDirection::Row),
        "row-reverse" => Some(FlexDirection::RowReverse),
        "column" => Some(FlexDirection::Column),
        "column-reverse" => Some(FlexDirection::ColumnReverse),
        _ => None,
    }
}

fn parse_gap(value: &str) -> Option<(f32, f32)> {
    let parsed: Vec<f32> = value
        .split_whitespace()
        .map(parse_non_negative_length)
        .collect::<Option<_>>()?;
    match parsed.as_slice() {
        [both] => Some((*both, *both)),
        [row, column] => Some((*row, *column)),
        _ => None,
    }
}

fn parse_border_width(value: &str) -> Option<f32> {
    let value = value.trim();
    if let Some(width) = parse_non_negative_length(value) {
        return Some(width);
    }
    for token in value.split_whitespace() {
        if let Some(width) =
            parse_border_width_keyword(token).or_else(|| parse_non_negative_length(token))
        {
            return Some(width);
        }
    }
    parse_border_width_keyword(value)
}

fn parse_border_width_keyword(value: &str) -> Option<f32> {
    match value.trim().to_ascii_lowercase().as_str() {
        "thin" => Some(1.0),
        "medium" => Some(3.0),
        "thick" => Some(5.0),
        _ => None,
    }
}

fn parse_color_or_currentcolor(value: &str, current_color: Color) -> Option<Color> {
    match ColorOrKeyword::parse(value).ok()? {
        ColorOrKeyword::Color(color) => Some(color.to_display_list()),
        ColorOrKeyword::CurrentColor => Some(current_color),
    }
}

fn parse_position(value: &str) -> Option<LayoutPosition> {
    match value.trim().to_ascii_lowercase().as_str() {
        "static" => Some(LayoutPosition::Static),
        "relative" => Some(LayoutPosition::Relative),
        "absolute" => Some(LayoutPosition::Absolute),
        "fixed" => Some(LayoutPosition::Fixed),
        _ => None,
    }
}

fn parse_overflow(value: &str) -> Option<LayoutOverflow> {
    let first = value.split_whitespace().next()?;
    match first.trim().to_ascii_lowercase().as_str() {
        "visible" => Some(LayoutOverflow::Visible),
        "hidden" => Some(LayoutOverflow::Hidden),
        "clip" => Some(LayoutOverflow::Clip),
        "scroll" => Some(LayoutOverflow::Scroll),
        "auto" => Some(LayoutOverflow::Auto),
        _ => None,
    }
}

fn merge_overflow(current: LayoutOverflow, next: LayoutOverflow) -> LayoutOverflow {
    if next.clips_contents() { next } else { current }
}

fn parse_inset(value: &str) -> Option<f32> {
    let value = value.trim();
    if value.eq_ignore_ascii_case("auto") {
        return None;
    }
    parse_length(value)
}

fn parse_length(value: &str) -> Option<f32> {
    let value = value.trim();
    if value == "0" || value == "+0" || value == "-0" {
        return Some(0.0);
    }
    value
        .strip_suffix("px")
        .or_else(|| value.strip_suffix("PX"))
        .and_then(|number| number.parse::<f32>().ok())
        .filter(|value| value.is_finite())
}
