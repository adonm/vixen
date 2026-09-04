//! CSSOM/computed-style host extension for the JS runtime.
//!
//! Retained CSSOM host objects resolve current stylesheet rules and computed
//! values through explicit `deno_core` ops. Rule mutation APIs remain outside
//! this read-only CSSOM subset.

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
    style_sheets: Vec<Vec<CssomRuleRecord>>,
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

pub(super) fn refresh(runtime: &mut deno_core::JsRuntime, page: &Page) -> Result<(), EngineError> {
    let host = cssom_host_state(page).map_err(|err| {
        EngineError::script(
            codes::SCRIPT_EVAL,
            format!("failed to refresh CSSOM host snapshot: {err}"),
        )
    })?;
    runtime
        .op_state()
        .borrow_mut()
        .put(CssomHost(Arc::new(host)));
    Ok(())
}

#[deno_core::op2]
#[serde]
fn op_vixen_cssom_snapshot(state: &mut OpState) -> deno_core::serde_json::Value {
    let host = state.borrow::<CssomHost>();
    json!({
        "styleSheetCount": host.0.style_sheet_count,
        "styleSheets": host.0.style_sheets.iter().map(|rules| {
            rules.iter().map(cssom_rule_value).collect::<Vec<_>>()
        }).collect::<Vec<_>>(),
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
    let style_sheets = style_blocks
        .iter()
        .map(|block| {
            let sheet = AuthorStylesheet::from_blocks(std::slice::from_ref(block));
            (0..sheet.rule_count())
                .filter_map(|index| sheet.rule(index).map(cssom_rule_record))
                .collect::<Vec<_>>()
        })
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
        style_sheets,
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
        .unwrap_or_else(|| initial_computed_style_value(property).to_owned())
}

fn initial_computed_style_value(property: &str) -> &'static str {
    match property.to_ascii_lowercase().as_str() {
        "visibility" => "visible",
        "cursor" => "auto",
        "opacity" => "1",
        "pointer-events" => "auto",
        _ => "",
    }
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
  const webidl = globalThis.__vixenWebidl;

  let styleSheets;
  const styleSheetByOwner = new WeakMap();

  function cssomSnapshot() {
    return op_vixen_cssom_snapshot();
  }

  function sheetRecords(index) {
    const sheets = cssomSnapshot().styleSheets || [];
    return sheets[index] || [];
  }

  function unwrapCssomOp(result) {
    if (!result.ok) throw new TypeError(result.message);
    return result;
  }

  function cssPropertyName(name) {
    const value = String(name);
    if (value === 'cssFloat') return 'float';
    return value.replace(/[A-Z]/g, (ch) => '-' + ch.toLowerCase()).replace(/_/g, '-');
  }

  function localComputedStyleValue(property) {
    const name = cssPropertyName(property);
    if (name === 'display') return 'inline';
    if (name === 'visibility') return 'visible';
    if (name === 'cursor') return 'auto';
    return '';
  }

  function dynamicStyleValue(nodeId, property) {
    const document = globalThis.document;
    if (!document || typeof document.querySelectorAll !== 'function') return null;
    const elements = document.querySelectorAll('*');
    let element = null;
    for (const candidate of elements) {
      if (candidate && candidate.__vixenNodeId === nodeId) {
        element = candidate;
        break;
      }
    }
    if (!element) return null;
    const name = cssPropertyName(property);
    let value = null;
    const styles = document.querySelectorAll('style');
    for (const style of styles) {
      if (!style || !(style.__vixenNodeId < 0)) continue;
      const cssText = String(style.textContent || '');
      const rulePattern = /([^{}]+)\{([^{}]+)\}/g;
      let match;
      while ((match = rulePattern.exec(cssText)) !== null) {
        const selector = match[1].trim();
        if (!selector || typeof element.matches !== 'function' || !element.matches(selector)) continue;
        for (const declaration of match[2].split(';')) {
          const index = declaration.indexOf(':');
          if (index === -1) continue;
          const prop = cssPropertyName(declaration.slice(0, index).trim());
          if (prop === name) value = declaration.slice(index + 1).replace(/!important\s*$/i, '').trim();
        }
      }
    }
    return value;
  }

  function computedStyleValue(nodeId, property) {
    if (Number(nodeId) < 0) return localComputedStyleValue(property);
    const dynamic = dynamicStyleValue(nodeId, property);
    if (dynamic !== null) return dynamic;
    return unwrapCssomOp(op_vixen_computed_style_property(nodeId, String(property))).value;
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
      const resolve = typeof declarations === 'function' ? declarations : (() => declarations);
      Object.defineProperty(this, '__vixenResolveDeclarations', {
        value: () => resolve().map(([name, value]) => [String(name), String(value)]),
        enumerable: false,
      });
      return new Proxy(this, {
        get(target, property, receiver) {
          if (typeof property === 'string' && /^(0|[1-9]\d*)$/.test(property)) {
            return target.item(Number(property));
          }
          return Reflect.get(target, property, receiver);
        },
        ownKeys(target) {
          const indexed = Array.from({ length: target.length }, (_, index) => String(index));
          return [...indexed, ...Reflect.ownKeys(target)];
        },
        getOwnPropertyDescriptor(target, property) {
          if (typeof property === 'string' && /^(0|[1-9]\d*)$/.test(property)) {
            const name = target.item(Number(property));
            if (name === '') return undefined;
            return { value: name, writable: false, enumerable: true, configurable: true };
          }
          return Reflect.getOwnPropertyDescriptor(target, property);
        },
      });
    }
    get __vixenDeclarations() { return this.__vixenResolveDeclarations(); }
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

  webidl.adoptInterface('CSSStyleDeclaration', VixenCSSStyleDeclaration);

  class VixenComputedStyle extends VixenCSSStyleDeclaration {
    constructor(nodeId) {
      super([]);
      Object.defineProperty(this, '__vixenNodeId', {
        value: nodeId,
        enumerable: false,
      });
    }
    getPropertyValue(property) {
      return computedStyleValue(this.__vixenNodeId, property);
    }
  }

  class VixenCSSStyleRule {
    constructor(sheet, ruleIndex) {
      Object.defineProperties(this, {
        __vixenSheet: { value: sheet, enumerable: false },
        __vixenRuleIndex: { value: ruleIndex, enumerable: false },
        style: {
          value: new VixenCSSStyleDeclaration(
            () => this.__vixenRecord.declarations || [],
          ),
          enumerable: true,
          configurable: true,
        },
      });
    }
    get __vixenRecord() {
      return sheetRecords(this.__vixenSheet.__vixenIndex)[this.__vixenRuleIndex] || {
        selectorText: '',
        cssText: '',
        declarations: [],
      };
    }
    get selectorText() { return this.__vixenRecord.selectorText; }
    get cssText() { return this.__vixenRecord.cssText; }
  }

  webidl.adoptInterface('CSSStyleRule', VixenCSSStyleRule);

  class VixenCSSRuleList {
    constructor(sheet) {
      Object.defineProperties(this, {
        __vixenSheet: { value: sheet, enumerable: false },
        __vixenRuleObjects: { value: new Map(), enumerable: false },
      });
      return new Proxy(this, {
        get(target, property, receiver) {
          if (typeof property === 'string' && /^(0|[1-9]\d*)$/.test(property)) {
            return target.item(Number(property));
          }
          return Reflect.get(target, property, receiver);
        },
        ownKeys(target) {
          const indexed = Array.from({ length: target.length }, (_, index) => String(index));
          return [...indexed, ...Reflect.ownKeys(target)];
        },
        getOwnPropertyDescriptor(target, property) {
          if (typeof property === 'string' && /^(0|[1-9]\d*)$/.test(property)) {
            const rule = target.item(Number(property));
            if (rule === null) return undefined;
            return { value: rule, writable: false, enumerable: true, configurable: true };
          }
          return Reflect.getOwnPropertyDescriptor(target, property);
        },
      });
    }
    get length() { return sheetRecords(this.__vixenSheet.__vixenIndex).length; }
    item(index) {
      const n = Number(index);
      if (!Number.isInteger(n) || n < 0 || n >= this.length) return null;
      if (!this.__vixenRuleObjects.has(n)) {
        this.__vixenRuleObjects.set(n, new VixenCSSStyleRule(this.__vixenSheet, n));
      }
      return this.__vixenRuleObjects.get(n);
    }
  }

  webidl.adoptInterface('CSSRuleList', VixenCSSRuleList);

  class VixenCSSStyleSheet {
    constructor(index, ownerNode = null) {
      Object.defineProperties(this, {
        __vixenIndex: { value: index, writable: true, enumerable: false },
        __vixenOwnerNode: { value: ownerNode, enumerable: false },
        __vixenDisabled: { value: false, writable: true, enumerable: false },
      });
      Object.defineProperty(this, 'cssRules', {
        value: new VixenCSSRuleList(this),
        enumerable: true,
        configurable: true,
      });
    }
    get disabled() { return this.__vixenDisabled; }
    set disabled(value) { this.__vixenDisabled = Boolean(value); }
    get href() { return null; }
    get ownerNode() {
      if (this.__vixenOwnerNode !== null) return this.__vixenOwnerNode;
      const document = globalThis.document;
      if (!document || typeof document.querySelectorAll !== 'function') return null;
      return document.querySelectorAll('style, link[rel~="stylesheet"]').item(this.__vixenIndex);
    }
  }

  webidl.adoptInterface('CSSStyleSheet', VixenCSSStyleSheet);

  function styleSheetOwner(index) {
    const document = globalThis.document;
    if (!document || typeof document.querySelectorAll !== 'function') return null;
    return document.querySelectorAll('style').item(index);
  }

  function styleSheetForOwner(ownerNode, create) {
    if (!ownerNode || (typeof ownerNode !== 'object' && typeof ownerNode !== 'function')) return null;
    let index = -1;
    const document = globalThis.document;
    if (document && typeof document.querySelectorAll === 'function') {
      const owners = document.querySelectorAll('style');
      for (let candidate = 0; candidate < owners.length; candidate++) {
        if (owners.item(candidate) === ownerNode) {
          index = candidate;
          break;
        }
      }
    }
    let sheet = styleSheetByOwner.get(ownerNode);
    if (sheet) {
      if (index >= 0) sheet.__vixenIndex = index;
      return sheet;
    }
    if (!create) return null;
    sheet = new VixenCSSStyleSheet(index, ownerNode);
    styleSheetByOwner.set(ownerNode, sheet);
    return sheet;
  }

  class VixenStyleSheetList {
    constructor() {
      Object.defineProperty(this, '__vixenSheetObjects', {
        value: new Map(),
        enumerable: false,
      });
      return new Proxy(this, {
        get(target, property, receiver) {
          if (typeof property === 'string' && /^(0|[1-9]\d*)$/.test(property)) {
            return target.item(Number(property));
          }
          return Reflect.get(target, property, receiver);
        },
        ownKeys(target) {
          const indexed = Array.from({ length: target.length }, (_, index) => String(index));
          return [...indexed, ...Reflect.ownKeys(target)];
        },
        getOwnPropertyDescriptor(target, property) {
          if (typeof property === 'string' && /^(0|[1-9]\d*)$/.test(property)) {
            const sheet = target.item(Number(property));
            if (sheet === null) return undefined;
            return { value: sheet, writable: false, enumerable: true, configurable: true };
          }
          return Reflect.getOwnPropertyDescriptor(target, property);
        },
      });
    }
    get length() { return cssomSnapshot().styleSheetCount; }
    item(index) {
      const n = Number(index);
      if (!Number.isInteger(n) || n < 0 || n >= this.length) return null;
      const owner = styleSheetOwner(n);
      if (owner !== null) return styleSheetForOwner(owner, true);
      if (!this.__vixenSheetObjects.has(n)) {
        this.__vixenSheetObjects.set(n, new VixenCSSStyleSheet(n));
      }
      return this.__vixenSheetObjects.get(n);
    }
  }

  webidl.adoptInterface('StyleSheetList', VixenStyleSheetList);

  function makeStyleSheets() {
    return new VixenStyleSheetList();
  }

  Object.defineProperty(globalThis, '__vixenCssomSheetForOwner', {
    value(ownerNode, create = false) { return styleSheetForOwner(ownerNode, Boolean(create)); },
    configurable: true,
  });

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
