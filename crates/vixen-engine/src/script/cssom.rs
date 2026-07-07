//! CSSOM/computed-style host extension for the JS runtime.
//!
//! The extension keeps the current read-only Phase 6 CSSOM smoke surface behind
//! explicit `deno_core` ops while the full WebIDL binding layer is still
//! landing.

#![forbid(unsafe_code)]

use std::borrow::Cow;
use std::sync::Arc;

use deno_core::serde_json::json;
use deno_core::{Extension, ExtensionFileSource, OpState};

use crate::engine_error::{EngineError, codes};
use crate::page::Page;
use crate::style_cascade::{AuthorStyleRule, AuthorStylesheet, css_supports};

struct CssomHost(Arc<CssomHostState>);

struct CssomHostState {
    style_sheet_count: usize,
    rules: Vec<CssomRuleRecord>,
    computed_styles: Vec<ComputedStyleRecord>,
}

struct CssomRuleRecord {
    selector_text: String,
    css_text: String,
    declarations: Vec<(String, String)>,
}

struct ComputedStyleRecord {
    node_id: usize,
    properties: Vec<(String, String)>,
}

deno_core::extension!(
    vixen_cssom,
    ops = [
        op_vixen_cssom_snapshot,
        op_vixen_css_supports,
        op_vixen_computed_style_property,
    ],
    options = {
        host: Arc<CssomHostState>,
    },
    state = |state, options| {
        state.put(CssomHost(options.host))
    },
);

pub(super) fn extension(page: &Page) -> Result<Extension, EngineError> {
    let host = cssom_host_state(page).map_err(|err| {
        EngineError::script(
            codes::SCRIPT_EVAL,
            format!("failed to build CSSOM host snapshot: {err}"),
        )
    })?;
    let mut extension = vixen_cssom::init(Arc::new(host));
    extension.js_files = Cow::Owned(vec![ExtensionFileSource::new_computed(
        "ext:vixen_cssom/bootstrap.js",
        Arc::<str>::from(CSSOM_API_BOOTSTRAP),
    )]);
    Ok(extension)
}

#[deno_core::op2]
#[serde]
fn op_vixen_cssom_snapshot(state: &mut OpState) -> deno_core::serde_json::Value {
    let host = state.borrow::<CssomHost>();
    json!({
        "styleSheetCount": host.0.style_sheet_count,
        "rules": host.0.rules.iter().map(cssom_rule_value).collect::<Vec<_>>(),
    })
}

#[deno_core::op2(fast)]
fn op_vixen_css_supports(#[string] condition: &str) -> bool {
    css_supports(condition)
}

#[deno_core::op2]
#[serde]
fn op_vixen_computed_style_property(
    state: &mut OpState,
    node_id: u32,
    #[string] property: String,
) -> deno_core::serde_json::Value {
    let host = state.borrow::<CssomHost>();
    let Some(record) = host
        .0
        .computed_styles
        .iter()
        .find(|record| record.node_id == node_id as usize)
    else {
        return json!({
            "ok": false,
            "message": format!("Vixen CSSOM host element is unavailable: {node_id}"),
        });
    };

    json!({
        "ok": true,
        "value": computed_style_property_value(&record.properties, &property),
    })
}

fn cssom_host_state(page: &Page) -> Result<CssomHostState, String> {
    let style_blocks = page.document().style_blocks();
    let stylesheet = AuthorStylesheet::from_blocks(&style_blocks);
    let rules = (0..stylesheet.rule_count())
        .filter_map(|index| stylesheet.rule(index).map(cssom_rule_record))
        .collect::<Vec<_>>();
    let computed_styles = page
        .query_selector_all("*")?
        .into_iter()
        .map(|info| ComputedStyleRecord {
            node_id: info.node_id,
            properties: page.computed_style(info.node_id),
        })
        .collect::<Vec<_>>();

    Ok(CssomHostState {
        style_sheet_count: style_blocks.len(),
        rules,
        computed_styles,
    })
}

fn cssom_rule_record(rule: AuthorStyleRule<'_>) -> CssomRuleRecord {
    let declarations = (0..rule.declaration_count())
        .filter_map(|index| {
            let property = rule.declaration_property(index)?.to_owned();
            let value = rule
                .get_property_value(&property)
                .unwrap_or_default()
                .to_owned();
            Some((property, value))
        })
        .collect();
    CssomRuleRecord {
        selector_text: rule.selector_text().to_owned(),
        css_text: rule.css_text(),
        declarations,
    }
}

fn cssom_rule_value(record: &CssomRuleRecord) -> deno_core::serde_json::Value {
    json!({
        "selectorText": &record.selector_text,
        "cssText": &record.css_text,
        "declarations": &record.declarations,
    })
}

fn computed_style_property_value(properties: &[(String, String)], property: &str) -> String {
    properties
        .iter()
        .find(|(name, _)| computed_style_property_matches(name, property))
        .map(|(_, value)| value.clone())
        .unwrap_or_default()
}

fn computed_style_property_matches(name: &str, property: &str) -> bool {
    if property.starts_with("--") {
        name == property
    } else {
        name.eq_ignore_ascii_case(property)
    }
}

const CSSOM_API_BOOTSTRAP: &str = r#"
(() => {
  const {
    op_vixen_cssom_snapshot,
    op_vixen_css_supports,
    op_vixen_computed_style_property,
  } = Deno.core.ops;

  const snapshot = op_vixen_cssom_snapshot();
  let styleSheets;

  function unwrapCssomOp(result) {
    if (!result.ok) throw new TypeError(result.message);
    return result;
  }

  function cssPropertyName(name) {
    const value = String(name);
    if (value === 'cssFloat') return 'float';
    return value.replace(/[A-Z]/g, (ch) => '-' + ch.toLowerCase()).replace(/_/g, '-');
  }

  function computedStyleValue(nodeId, property) {
    return unwrapCssomOp(op_vixen_computed_style_property(nodeId, String(property))).value;
  }

  class VixenComputedStyle {
    constructor(nodeId) {
      Object.defineProperty(this, '__vixenNodeId', {
        value: nodeId,
        enumerable: false,
      });
    }
    getPropertyValue(property) {
      return computedStyleValue(this.__vixenNodeId, property);
    }
  }

  const computedStyleHandler = {
    get(target, property, receiver) {
      if (typeof property === 'symbol') return Reflect.get(target, property, receiver);
      if (property in target) {
        const value = Reflect.get(target, property, receiver);
        return typeof value === 'function' ? value.bind(target) : value;
      }
      return target.getPropertyValue(cssPropertyName(property));
    },
  };

  class VixenCSSStyleDeclaration {
    constructor(declarations) {
      Object.defineProperty(this, '__vixenDeclarations', {
        value: Object.freeze(declarations.map(([name, value]) => Object.freeze([name, value]))),
        enumerable: false,
      });
      for (let i = 0; i < this.__vixenDeclarations.length; i++) {
        Object.defineProperty(this, String(i), {
          value: this.__vixenDeclarations[i][0],
          enumerable: true,
          configurable: true,
        });
      }
    }
    get length() { return this.__vixenDeclarations.length; }
    item(index) {
      const n = Number(index);
      return Number.isInteger(n) && n >= 0 && n < this.__vixenDeclarations.length
        ? this.__vixenDeclarations[n][0]
        : '';
    }
    getPropertyValue(property) {
      const name = String(property);
      const pair = this.__vixenDeclarations.find(([prop]) => prop === name);
      return pair ? pair[1] : '';
    }
  }

  class VixenCSSStyleRule {
    constructor(record) {
      Object.defineProperty(this, '__vixenRecord', {
        value: record,
        enumerable: false,
      });
      Object.defineProperty(this, 'style', {
        value: new VixenCSSStyleDeclaration(record.declarations),
        enumerable: true,
        configurable: true,
      });
    }
    get selectorText() { return this.__vixenRecord.selectorText; }
    get cssText() { return this.__vixenRecord.cssText; }
  }

  function makeRuleList(records) {
    const rules = records.map((record) => new VixenCSSStyleRule(record));
    Object.defineProperty(rules, 'item', {
      value(index) {
        const n = Number(index);
        return Number.isInteger(n) && n >= 0 && n < rules.length ? rules[n] : null;
      },
      configurable: true,
    });
    return rules;
  }

  function makeStyleSheet(index) {
    const rules = index === 0 ? snapshot.rules : [];
    return {
      disabled: false,
      href: null,
      ownerNode: { tagName: 'STYLE' },
      cssRules: makeRuleList(rules),
    };
  }

  function makeStyleSheets() {
    const sheets = [];
    for (let i = 0; i < snapshot.styleSheetCount; i++) {
      sheets.push(makeStyleSheet(i));
    }
    Object.defineProperty(sheets, 'item', {
      value(index) {
        const n = Number(index);
        return Number.isInteger(n) && n >= 0 && n < sheets.length ? sheets[n] : null;
      },
      configurable: true,
    });
    return sheets;
  }

  function getComputedStyle(element) {
    if (!element || typeof element.__vixenNodeId !== 'number') {
      throw new TypeError('getComputedStyle expects a Vixen Element');
    }
    return new Proxy(new VixenComputedStyle(element.__vixenNodeId), computedStyleHandler);
  }

  if (typeof globalThis.window === 'undefined') {
    Object.defineProperty(globalThis, 'window', {
      value: globalThis,
      writable: true,
      configurable: true,
    });
  }

  const cssObject = typeof globalThis.CSS === 'object' && globalThis.CSS !== null ? globalThis.CSS : {};
  Object.defineProperty(cssObject, 'supports', {
    value(...args) {
      if (args.length < 1) throw new TypeError('CSS.supports requires an argument');
      const condition = args.length >= 2 ? '(' + String(args[0]) + ': ' + String(args[1]) + ')' : String(args[0]);
      return op_vixen_css_supports(condition);
    },
    writable: true,
    configurable: true,
  });
  Object.defineProperty(globalThis, 'CSS', {
    value: cssObject,
    writable: true,
    configurable: true,
  });

  Object.defineProperty(globalThis, 'getComputedStyle', {
    value: getComputedStyle,
    writable: true,
    configurable: true,
  });

  if (typeof globalThis.document === 'object' && globalThis.document !== null) {
    Object.defineProperty(globalThis.document, 'styleSheets', {
      get() {
        if (!styleSheets) styleSheets = makeStyleSheets();
        return styleSheets;
      },
      enumerable: true,
      configurable: true,
    });
  }
})();
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cssom_data_crosses_ops_and_bootstrap_stays_ascii() {
        assert!(CSSOM_API_BOOTSTRAP.is_ascii());
        assert!(CSSOM_API_BOOTSTRAP.contains("op_vixen_cssom_snapshot"));
        assert!(CSSOM_API_BOOTSTRAP.contains("op_vixen_css_supports"));
        assert!(CSSOM_API_BOOTSTRAP.contains("op_vixen_computed_style_property"));

        let page = Page::from_html(
            "file:///cssom-op.html",
            "<style>#copy { color: blue; font-size: 20px !important; --Token: A:B; }</style><p id='copy' style='font-size: 18px; margin-left: 10px'>Text</p>",
        )
        .unwrap();
        let host = cssom_host_state(&page).unwrap();

        assert_eq!(host.style_sheet_count, 1);
        assert_eq!(host.rules.len(), 1);
        assert_eq!(host.rules[0].selector_text, "#copy");
        assert_eq!(host.rules[0].declarations[0].0, "color");

        let copy = page.query_selector_all("#copy").unwrap().remove(0);
        let record = host
            .computed_styles
            .iter()
            .find(|record| record.node_id == copy.node_id)
            .unwrap();
        assert_eq!(
            computed_style_property_value(&record.properties, "color"),
            "blue"
        );
        assert_eq!(
            computed_style_property_value(&record.properties, "font-size"),
            "20px"
        );
        assert_eq!(
            computed_style_property_value(&record.properties, "--Token"),
            "A:B"
        );
    }
}
