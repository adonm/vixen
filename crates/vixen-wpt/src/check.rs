//! The WPT check types (docs/SPEC.md "WPT harness — check types") and the
//! per-check runner. Checks map to [`HarnessEngine`]'s inspection surface;
//! `ref-equivalent` compares the stable display-list render projection;
//! `visual-hash` consumes RGBA screenshots from adapters with an offscreen
//! renderer.

use serde::{Deserialize, Serialize};
use vixen_api::PageSnapshot;

use crate::harness::HarnessEngine;
use crate::visual_hash::{VisualHash, hash_rgba};

/// Default viewport the harness inspects at (matches `vixen-headless`).
const VW: u32 = 800;
const VH: u32 = 600;

/// Result of running one check against a fixture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Pass,
    Fail(String),
    /// The check could not run (the engine capability isn't wired yet).
    Skipped(String),
}

/// One assertion. `#[serde(tag = "type")]` ⇒ `{ "type": "title", "expected": "X" }`.
/// `type` names are kebab-cased to match the SPEC table exactly.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Check {
    Title {
        expected: String,
    },
    SelectorCount {
        selector: String,
        expected: usize,
    },
    SelectorsExact {
        selector: String,
        expected: Vec<String>,
    },
    BodyContains {
        expected: String,
    },
    JsEval {
        expr: String,
        expected: String,
    },
    MinNodes {
        min: usize,
    },
    NoCriticalDiagnostics,
    VisualHash {
        expected: String,
    },
    SelectorMatch {
        selector: String,
        expected: Vec<String>,
    },
    ComputedStyle {
        selector: String,
        property: String,
        expected: String,
    },
    ElementAttribute {
        selector: String,
        attribute: String,
        expected: String,
    },
    LayoutBox {
        selector: String,
        expected: [f64; 4],
        #[serde(default = "default_layout_tolerance")]
        tolerance: f64,
    },
    DisplayListContains {
        expected: String,
    },
    DomNodesRange {
        min: usize,
        max: usize,
    },
    RefEquivalent {
        reference: String,
    },
}

impl Check {
    /// Run this check against `engine`, returning an [`Outcome`].
    pub fn run(&self, engine: &dyn HarnessEngine) -> Outcome {
        match self {
            Check::Title { expected } => {
                let snap = engine.snapshot(VW, VH);
                match snap.title {
                    Some(t) if t == *expected => Outcome::Pass,
                    Some(t) => Outcome::Fail(format!("title: expected {expected:?}, got {t:?}")),
                    None => Outcome::Fail(format!("title: expected {expected:?}, got <none>")),
                }
            }
            Check::BodyContains { expected } => {
                let snap = engine.snapshot(VW, VH);
                if snap.text_content.contains(expected) {
                    Outcome::Pass
                } else {
                    Outcome::Fail("body does not contain expected substring".into())
                }
            }
            Check::MinNodes { min } => {
                let n = engine.snapshot(VW, VH).element_count;
                if n >= *min {
                    Outcome::Pass
                } else {
                    Outcome::Fail(format!("min-nodes: expected ≥{min}, got {n}"))
                }
            }
            Check::DomNodesRange { min, max } => {
                let n = engine.snapshot(VW, VH).element_count;
                if (*min..=*max).contains(&n) {
                    Outcome::Pass
                } else {
                    Outcome::Fail(format!("dom-nodes-range: expected {min}..={max}, got {n}"))
                }
            }
            Check::NoCriticalDiagnostics => {
                // docs/SPEC.md "Diagnostics shape" has no severity field; until
                // the Phase 7 taxonomy lands, any diagnostic is treated as
                // critical (fail-closed).
                let diags = engine.diagnostics();
                if diags.is_empty() {
                    Outcome::Pass
                } else {
                    Outcome::Fail(format!("no-critical-diagnostics: {} recorded", diags.len()))
                }
            }
            Check::SelectorCount { selector, expected } => {
                match engine.query_selector_all(selector) {
                    Ok(matches) => {
                        let got = matches.len();
                        if got == *expected {
                            Outcome::Pass
                        } else {
                            Outcome::Fail(format!("selector-count: expected {expected}, got {got}"))
                        }
                    }
                    Err(e) => Outcome::Fail(format!("selector-count: {e}")),
                }
            }
            Check::SelectorsExact { selector, expected } => {
                match engine.query_selector_all(selector) {
                    Ok(matches) => {
                        let mut got: Vec<String> =
                            matches.iter().filter_map(|e| e.id.clone()).collect();
                        got.sort();
                        let mut exp = expected.clone();
                        exp.sort();
                        if got == exp {
                            Outcome::Pass
                        } else {
                            Outcome::Fail(format!("selectors-exact: expected {exp:?}, got {got:?}"))
                        }
                    }
                    Err(e) => Outcome::Fail(format!("selectors-exact: {e}")),
                }
            }
            Check::SelectorMatch { selector, expected } => {
                // "Per-element selector match details": compare matched element
                // tags in document order (a stable, inspectable projection).
                match engine.query_selector_all(selector) {
                    Ok(matches) => {
                        let got: Vec<String> = matches.iter().map(|e| e.tag.clone()).collect();
                        if got == *expected {
                            Outcome::Pass
                        } else {
                            Outcome::Fail(format!(
                                "selector-match: expected {expected:?}, got {got:?}"
                            ))
                        }
                    }
                    Err(e) => Outcome::Fail(format!("selector-match: {e}")),
                }
            }
            Check::ComputedStyle {
                selector,
                property,
                expected,
            } => match first_match(engine, selector) {
                Some(node_id) => {
                    let styles = engine.computed_style(node_id);
                    let got = styles
                        .iter()
                        .find(|(k, _)| k == property)
                        .map(|(_, v)| v.clone());
                    match got {
                        Some(v) if v == *expected => Outcome::Pass,
                        Some(v) => Outcome::Fail(format!(
                            "computed-style {property}: expected {expected:?}, got {v:?}"
                        )),
                        None => Outcome::Fail(format!(
                            "computed-style: property {property:?} not present"
                        )),
                    }
                }
                None => Outcome::Fail("computed-style: selector matched nothing".into()),
            },
            Check::ElementAttribute {
                selector,
                attribute,
                expected,
            } => match first_match_info(engine, selector) {
                Some(info) => {
                    let got = info
                        .attributes
                        .iter()
                        .find(|(k, _)| k == attribute)
                        .map(|(_, v)| v.clone());
                    match got {
                        Some(v) if v == *expected => Outcome::Pass,
                        Some(v) => Outcome::Fail(format!(
                            "element-attribute {attribute}: expected {expected:?}, got {v:?}"
                        )),
                        None => Outcome::Fail(format!(
                            "element-attribute: attribute {attribute:?} not present"
                        )),
                    }
                }
                None => Outcome::Fail("element-attribute: selector matched nothing".into()),
            },
            Check::LayoutBox {
                selector,
                expected,
                tolerance,
            } => match first_match_info(engine, selector) {
                Some(info) => match info.bbox {
                    Some(got) if box_matches(got, *expected, *tolerance) => Outcome::Pass,
                    Some(got) => Outcome::Fail(format!(
                        "layout-box: expected {:?} ±{}, got {:?}",
                        expected, tolerance, got
                    )),
                    None => Outcome::Fail("layout-box: element has no layout bbox".into()),
                },
                None => Outcome::Fail("layout-box: selector matched nothing".into()),
            },
            Check::JsEval { expr, expected } => match engine.eval(expr) {
                Ok(got) if got == *expected => Outcome::Pass,
                Ok(got) => Outcome::Fail(format!("js-eval: expected {expected:?}, got {got:?}")),
                Err(e) => Outcome::Fail(format!("js-eval: {e}")),
            },
            Check::DisplayListContains { expected } => match engine.display_list(VW, VH) {
                Ok(dump) if dump.contains(expected) => Outcome::Pass,
                Ok(_) => Outcome::Fail("display-list does not contain expected substring".into()),
                Err(e) => Outcome::Fail(format!("display-list: {e}")),
            },
            Check::VisualHash { expected } => match engine.screenshot_rgba(VW, VH) {
                Err(_) => Outcome::Skipped("needs offscreen renderer (Phase 5)".into()),
                Ok(screenshot) => {
                    let expected = match expected.parse::<VisualHash>() {
                        Ok(hash) => hash,
                        Err(err) => return Outcome::Fail(format!("visual-hash: {err}")),
                    };
                    match hash_rgba(screenshot.width, screenshot.height, &screenshot.rgba) {
                        Some(actual) if expected.matches(actual) => Outcome::Pass,
                        Some(actual) => Outcome::Fail(format!(
                            "visual-hash: expected {expected}, got {actual} (distance {}, tolerance {})",
                            expected.distance(actual),
                            expected.tolerance
                        )),
                        None => Outcome::Fail("visual-hash: invalid RGBA screenshot buffer".into()),
                    }
                }
            },
            Check::RefEquivalent { reference } => match (
                engine.display_list(VW, VH),
                engine.reference_display_list(reference, VW, VH),
            ) {
                (Ok(got), Ok(expected)) if render_dumps_match(&got, &expected) => Outcome::Pass,
                (Ok(got), Ok(expected)) => Outcome::Fail(format!(
                    "ref-equivalent {reference}: render dumps differ ({})",
                    first_render_dump_difference(&got, &expected)
                )),
                (Err(e), _) => Outcome::Fail(format!("ref-equivalent: {e}")),
                (_, Err(e)) => Outcome::Fail(format!("ref-equivalent reference {reference}: {e}")),
            },
        }
    }
}

fn default_layout_tolerance() -> f64 {
    0.1
}

fn box_matches(got: (f64, f64, f64, f64), expected: [f64; 4], tolerance: f64) -> bool {
    let got = [got.0, got.1, got.2, got.3];
    got.iter()
        .zip(expected)
        .all(|(got, expected)| (got - expected).abs() <= tolerance)
}

/// First match's node id for `selector`, if any.
fn first_match(engine: &dyn HarnessEngine, selector: &str) -> Option<usize> {
    engine
        .query_selector_all(selector)
        .ok()?
        .into_iter()
        .next()
        .map(|e| e.node_id)
}

fn first_match_info(engine: &dyn HarnessEngine, selector: &str) -> Option<vixen_api::ElementInfo> {
    engine.query_selector_all(selector).ok()?.into_iter().next()
}

fn render_dumps_match(got: &str, expected: &str) -> bool {
    got.trim_end() == expected.trim_end()
}

fn first_render_dump_difference(got: &str, expected: &str) -> String {
    for (line, (got, expected)) in got.lines().zip(expected.lines()).enumerate() {
        if got != expected {
            return format!("line {}: got {got:?}, expected {expected:?}", line + 1);
        }
    }
    format!(
        "line count differs: got {}, expected {}",
        got.lines().count(),
        expected.lines().count()
    )
}

// `PageSnapshot` is referenced in trait bounds/docs; keep the import live.
#[allow(dead_code)]
fn _snapshot_bound(_: &PageSnapshot) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::{MockEngine, RgbaScreenshot};
    use vixen_api::{ElementInfo, EngineDiagnostic, EngineDiagnosticCategory, PageSnapshot};

    fn snap(title: Option<&str>, text: &str, n: usize) -> PageSnapshot {
        PageSnapshot {
            url: "https://example.test/".into(),
            title: title.map(str::to_owned),
            viewport: (800, 600),
            text_content: text.into(),
            element_count: n,
            root_scroll: (0.0, 0.0),
            root_scroll_max: (0.0, 0.0),
        }
    }

    #[test]
    fn title_body_minnodes_range() {
        let e = MockEngine {
            snapshot: snap(Some("Hi"), "hello world body", 10),
            ..Default::default()
        };
        assert_eq!(
            Check::Title {
                expected: "Hi".into()
            }
            .run(&e),
            Outcome::Pass
        );
        assert_eq!(
            Check::Title {
                expected: "No".into()
            }
            .run(&e),
            Outcome::Fail("title: expected \"No\", got \"Hi\"".into())
        );
        assert_eq!(
            Check::BodyContains {
                expected: "world".into()
            }
            .run(&e),
            Outcome::Pass
        );
        assert!(
            Check::BodyContains {
                expected: "missing".into()
            }
            .run(&e)
            .is_fail()
        );
        assert_eq!(Check::MinNodes { min: 5 }.run(&e), Outcome::Pass);
        assert!(Check::MinNodes { min: 50 }.run(&e).is_fail());
        assert_eq!(
            Check::DomNodesRange { min: 5, max: 20 }.run(&e),
            Outcome::Pass
        );
        assert!(Check::DomNodesRange { min: 20, max: 30 }.run(&e).is_fail());
    }

    #[test]
    fn no_critical_diagnostics() {
        let e = MockEngine::default();
        assert_eq!(Check::NoCriticalDiagnostics.run(&e), Outcome::Pass);
        let mut e = MockEngine::default();
        e.diagnostics.push(EngineDiagnostic::new(
            EngineDiagnosticCategory::ScriptRuntime,
            "script.eval",
            "boom",
        ));
        assert!(Check::NoCriticalDiagnostics.run(&e).is_fail());
    }

    #[test]
    fn selector_checks() {
        let mut e = MockEngine::default();
        e.matches.insert(
            "div".into(),
            vec![
                ElementInfo {
                    node_id: 1,
                    tag: "div".into(),
                    id: Some("a".into()),
                    classes: vec![],
                    attributes: vec![("data-k".into(), "v".into())],
                    text: "".into(),
                    bbox: Some((10.0, 20.0, 30.0, 40.0)),
                },
                ElementInfo {
                    node_id: 2,
                    tag: "div".into(),
                    id: Some("b".into()),
                    classes: vec![],
                    attributes: vec![],
                    text: "".into(),
                    bbox: None,
                },
            ],
        );
        e.styles.insert(1, vec![("color".into(), "red".into())]);

        assert_eq!(
            Check::SelectorCount {
                selector: "div".into(),
                expected: 2
            }
            .run(&e),
            Outcome::Pass
        );
        assert_eq!(
            Check::SelectorsExact {
                selector: "div".into(),
                expected: vec!["b".into(), "a".into()]
            }
            .run(&e),
            Outcome::Pass
        );
        assert_eq!(
            Check::SelectorMatch {
                selector: "div".into(),
                expected: vec!["div".into(), "div".into()]
            }
            .run(&e),
            Outcome::Pass
        );
        assert_eq!(
            Check::ComputedStyle {
                selector: "div".into(),
                property: "color".into(),
                expected: "red".into()
            }
            .run(&e),
            Outcome::Pass
        );
        assert_eq!(
            Check::ElementAttribute {
                selector: "div".into(),
                attribute: "data-k".into(),
                expected: "v".into()
            }
            .run(&e),
            Outcome::Pass
        );
        assert_eq!(
            Check::LayoutBox {
                selector: "div".into(),
                expected: [10.0, 20.0, 30.0, 40.0],
                tolerance: 0.1,
            }
            .run(&e),
            Outcome::Pass
        );
        assert!(
            Check::LayoutBox {
                selector: "div".into(),
                expected: [10.0, 20.0, 31.0, 40.0],
                tolerance: 0.1,
            }
            .run(&e)
            .is_fail()
        );
    }

    #[test]
    fn js_eval_display_list_and_ref_equivalent() {
        let mut e = MockEngine {
            eval_result: Some(Ok("3".into())),
            display_list: Some(Ok("cmd 1: text x=0.0 y=0.0 w=40.0 h=10.0".into())),
            ..Default::default()
        };
        e.reference_display_lists.insert(
            "same.html".into(),
            Ok("cmd 1: text x=0.0 y=0.0 w=40.0 h=10.0".into()),
        );
        e.reference_display_lists.insert(
            "different.html".into(),
            Ok("cmd 1: text x=0.0 y=0.0 w=41.0 h=10.0".into()),
        );
        assert_eq!(
            Check::JsEval {
                expr: "1+2".into(),
                expected: "3".into()
            }
            .run(&e),
            Outcome::Pass
        );
        assert_eq!(
            Check::DisplayListContains {
                expected: "w=40.0 h=10.0".into()
            }
            .run(&e),
            Outcome::Pass
        );
        assert_eq!(
            Check::RefEquivalent {
                reference: "same.html".into()
            }
            .run(&e),
            Outcome::Pass
        );
        assert!(
            Check::RefEquivalent {
                reference: "different.html".into()
            }
            .run(&e)
            .is_fail()
        );
        assert!(matches!(
            Check::VisualHash {
                expected: "x".into()
            }
            .run(&e),
            Outcome::Skipped(_)
        ));
    }

    #[test]
    fn visual_hash_uses_rgba_screenshots_when_available() {
        let mut rgba = Vec::new();
        for _y in 0..8 {
            for x in 0..8 {
                let value = if x < 4 { 0 } else { 255 };
                rgba.extend_from_slice(&[value, value, value, 255]);
            }
        }
        let e = MockEngine {
            screenshot: Some(Ok(RgbaScreenshot {
                width: 8,
                height: 8,
                rgba,
            })),
            ..Default::default()
        };

        assert_eq!(
            Check::VisualHash {
                expected: "0f0f0f0f0f0f0f0f".into()
            }
            .run(&e),
            Outcome::Pass
        );
        assert!(
            Check::VisualHash {
                expected: "f0f0f0f0f0f0f0f0@0".into()
            }
            .run(&e)
            .is_fail()
        );
        assert!(
            Check::VisualHash {
                expected: "bad".into()
            }
            .run(&e)
            .is_fail()
        );
    }

    #[test]
    fn check_round_trips_json() {
        // Manifests are JSON; every variant must (de)serialize with its tag.
        let cases = [
            r#"{"type":"title","expected":"T"}"#,
            r#"{"type":"selector-count","selector":"div","expected":2}"#,
            r#"{"type":"selectors-exact","selector":"div","expected":["a"]}"#,
            r#"{"type":"body-contains","expected":"x"}"#,
            r#"{"type":"js-eval","expr":"1+2","expected":"3"}"#,
            r#"{"type":"min-nodes","min":3}"#,
            r#"{"type":"no-critical-diagnostics"}"#,
            r#"{"type":"visual-hash","expected":"h"}"#,
            r#"{"type":"selector-match","selector":"div","expected":["div"]}"#,
            r##"{"type":"computed-style","selector":"#x","property":"color","expected":"red"}"##,
            r##"{"type":"element-attribute","selector":"#x","attribute":"data-k","expected":"v"}"##,
            r##"{"type":"layout-box","selector":"#x","expected":[1.0,2.0,3.0,4.0]}"##,
            r#"{"type":"display-list-contains","expected":"cmd 1"}"#,
            r#"{"type":"dom-nodes-range","min":1,"max":5}"#,
            r#"{"type":"ref-equivalent","reference":"r.html"}"#,
        ];
        for json in cases {
            let check: Check = serde_json::from_str(json).unwrap_or_else(|e| panic!("{json}: {e}"));
            let reser = serde_json::to_string(&check).unwrap();
            let back: Check = serde_json::from_str(&reser).unwrap();
            assert_eq!(check, back, "round-trip mismatch for {json}");
        }
    }

    // small helper on Outcome for assertions
    impl Outcome {
        fn is_fail(&self) -> bool {
            matches!(self, Outcome::Fail(_))
        }
    }
}
