//! Generated WebIDL binding substrate for browser host objects.
//!
//! This is the first generated-binding layer: Rust owns an interface/member
//! manifest, renders deterministic bootstrap JS, and host-family extensions bind
//! their concrete implementations onto those generated prototypes. It is not a
//! complete DOM backend yet; it replaces ad-hoc constructor/prototype setup for
//! the currently runtime-visible browser API slice and gives later WebIDL import
//! work one place to expand.

#![forbid(unsafe_code)]

use std::borrow::Cow;
use std::sync::Arc;

use deno_core::{Extension, ExtensionFileSource};

deno_core::extension!(vixen_webidl);

pub(super) fn extension() -> Extension {
    let mut extension = vixen_webidl::init();
    extension.js_files = Cow::Owned(vec![ExtensionFileSource::new_computed(
        "ext:vixen_webidl/generated.js",
        Arc::<str>::from(generated_webidl_bootstrap()),
    )]);
    extension
}

#[cfg(test)]
pub(super) fn manifest_interface_names() -> Vec<&'static str> {
    WEBIDL_INTERFACES
        .iter()
        .map(|interface| interface.name)
        .collect()
}

#[cfg(test)]
pub(super) fn manifest_parent_pairs() -> Vec<(&'static str, &'static str)> {
    WEBIDL_INTERFACES
        .iter()
        .filter_map(|interface| interface.parent.map(|parent| (interface.name, parent)))
        .collect()
}

#[derive(Clone, Copy)]
struct WebIdlInterface {
    name: &'static str,
    parent: Option<&'static str>,
    attributes: &'static [&'static str],
    operations: &'static [&'static str],
}

macro_rules! iface {
    ($name:literal) => {
        WebIdlInterface {
            name: $name,
            parent: None,
            attributes: &[],
            operations: &[],
        }
    };
    ($name:literal => $parent:literal) => {
        WebIdlInterface {
            name: $name,
            parent: Some($parent),
            attributes: &[],
            operations: &[],
        }
    };
    ($name:literal; attrs [$($attr:literal),* $(,)?]; ops [$($op:literal),* $(,)?]) => {
        WebIdlInterface {
            name: $name,
            parent: None,
            attributes: &[$($attr),*],
            operations: &[$($op),*],
        }
    };
    ($name:literal => $parent:literal; attrs [$($attr:literal),* $(,)?]; ops [$($op:literal),* $(,)?]) => {
        WebIdlInterface {
            name: $name,
            parent: Some($parent),
            attributes: &[$($attr),*],
            operations: &[$($op),*],
        }
    };
}

const WEBIDL_INTERFACES: &[WebIdlInterface] = &[
    iface!("Window" => "EventTarget"; attrs ["document", "navigator", "location", "history", "screen", "performance", "innerWidth", "innerHeight", "devicePixelRatio"]; ops ["getComputedStyle", "matchMedia", "requestAnimationFrame", "cancelAnimationFrame", "fetch", "setTimeout", "clearTimeout", "setInterval", "clearInterval"]),
    WebIdlInterface {
        name: "EventTarget",
        parent: None,
        attributes: &[],
        operations: &["addEventListener", "removeEventListener", "dispatchEvent"],
    },
    WebIdlInterface {
        name: "Node",
        parent: Some("EventTarget"),
        attributes: &[
            "nodeName",
            "nodeType",
            "textContent",
            "ownerDocument",
            "isConnected",
            "parentNode",
            "childNodes",
            "firstChild",
            "lastChild",
            "previousSibling",
            "nextSibling",
        ],
        operations: &["contains", "getRootNode"],
    },
    iface!("CharacterData" => "Node"; attrs ["data", "length"]; ops ["substringData", "appendData", "insertData", "deleteData", "replaceData"]),
    iface!("Text" => "CharacterData"; attrs ["wholeText"]; ops ["splitText"]),
    iface!("CDATASection" => "Text"),
    iface!("Comment" => "CharacterData"),
    iface!("DocumentType" => "Node"; attrs ["name", "publicId", "systemId"]; ops []),
    iface!("DocumentFragment" => "Node"; attrs ["children", "firstElementChild", "lastElementChild", "childElementCount"]; ops ["querySelector", "querySelectorAll", "getElementById"]),
    iface!("ShadowRoot" => "DocumentFragment"; attrs ["mode", "host", "innerHTML"]; ops []),
    WebIdlInterface {
        name: "Element",
        parent: Some("Node"),
        attributes: &[
            "id",
            "className",
            "tagName",
            "localName",
            "classList",
            "attributes",
            "innerHTML",
            "outerHTML",
            "children",
            "parentElement",
            "firstElementChild",
            "lastElementChild",
            "childElementCount",
            "previousElementSibling",
            "nextElementSibling",
            "clientWidth",
            "clientHeight",
            "clientTop",
            "clientLeft",
            "scrollWidth",
            "scrollHeight",
            "scrollTop",
            "scrollLeft",
            "offsetWidth",
            "offsetHeight",
            "offsetTop",
            "offsetLeft",
            "offsetParent",
        ],
        operations: &[
            "getAttribute",
            "hasAttribute",
            "setAttribute",
            "removeAttribute",
            "getAttributeNames",
            "hasAttributes",
            "matches",
            "closest",
            "querySelector",
            "querySelectorAll",
            "append",
            "prepend",
            "replaceChildren",
            "attachShadow",
            "scrollIntoView",
            "getBoundingClientRect",
            "getClientRects",
            "getBoxQuads",
        ],
    },
    iface!("Attr"; attrs ["namespaceURI", "prefix", "localName", "name", "value", "ownerElement"]; ops []),
    iface!("NamedNodeMap"; attrs ["length"]; ops ["item", "getNamedItem", "setNamedItem", "removeNamedItem"]),
    iface!("HTMLCollection"; attrs ["length"]; ops ["item", "namedItem"]),
    iface!("NodeList"; attrs ["length"]; ops ["item", "entries", "keys", "values", "forEach"]),
    WebIdlInterface {
        name: "HTMLElement",
        parent: Some("Element"),
        attributes: &[
            "innerText",
            "dataset",
            "style",
            "hidden",
            "tabIndex",
            "accessKey",
            "accessKeyLabel",
            "draggable",
            "spellcheck",
            "translate",
            "inputMode",
            "enterKeyHint",
            "popover",
            "title",
            "lang",
            "dir",
        ],
        operations: &["click", "focus", "blur"],
    },
    iface!("HTMLHtmlElement" => "HTMLElement"),
    iface!("HTMLHeadElement" => "HTMLElement"),
    iface!("HTMLBodyElement" => "HTMLElement"; attrs ["onload", "onerror"]; ops []),
    iface!("HTMLTitleElement" => "HTMLElement"; attrs ["text"]; ops []),
    iface!("HTMLMetaElement" => "HTMLElement"; attrs ["name", "httpEquiv", "content", "charset"]; ops []),
    iface!("HTMLBaseElement" => "HTMLElement"; attrs ["href", "target"]; ops []),
    WebIdlInterface {
        name: "HTMLLinkElement",
        parent: Some("HTMLElement"),
        attributes: &[
            "href",
            "rel",
            "relList",
            "media",
            "type",
            "as",
            "crossOrigin",
        ],
        operations: &[],
    },
    iface!("HTMLStyleElement" => "HTMLElement"; attrs ["media", "type", "disabled", "sheet"]; ops []),
    iface!("HTMLScriptElement" => "HTMLElement"; attrs ["src", "type", "async", "defer", "crossOrigin", "text"]; ops ["supports"]),
    iface!("HTMLTemplateElement" => "HTMLElement"; attrs ["content"]; ops []),
    iface!("HTMLSlotElement" => "HTMLElement"; attrs ["name"]; ops ["assignedNodes", "assignedElements"]),
    iface!("HTMLDivElement" => "HTMLElement"),
    iface!("HTMLSpanElement" => "HTMLElement"),
    iface!("HTMLParagraphElement" => "HTMLElement"),
    iface!("HTMLHeadingElement" => "HTMLElement"),
    iface!("HTMLPreElement" => "HTMLElement"),
    iface!("HTMLQuoteElement" => "HTMLElement"; attrs ["cite"]; ops []),
    iface!("HTMLOListElement" => "HTMLElement"; attrs ["reversed", "start", "type"]; ops []),
    iface!("HTMLUListElement" => "HTMLElement"),
    iface!("HTMLLIElement" => "HTMLElement"; attrs ["value"]; ops []),
    iface!("HTMLAnchorElement" => "HTMLElement"; attrs ["href", "target", "download", "rel", "relList", "hreflang", "type", "origin", "protocol", "host", "hostname", "port", "pathname", "search", "hash"]; ops []),
    iface!("HTMLAreaElement" => "HTMLElement"; attrs ["alt", "coords", "shape", "href", "target", "download", "rel", "relList"]; ops []),
    iface!("HTMLBRElement" => "HTMLElement"),
    iface!("HTMLHRElement" => "HTMLElement"),
    iface!("HTMLDListElement" => "HTMLElement"),
    iface!("HTMLDataElement" => "HTMLElement"; attrs ["value"]; ops []),
    iface!("HTMLTimeElement" => "HTMLElement"; attrs ["dateTime"]; ops []),
    iface!("HTMLModElement" => "HTMLElement"; attrs ["cite", "dateTime"]; ops []),
    iface!("HTMLImageElement" => "HTMLElement"; attrs ["alt", "src", "srcset", "sizes", "crossOrigin", "useMap", "isMap", "width", "height", "naturalWidth", "naturalHeight", "complete", "currentSrc", "loading", "decoding"]; ops ["decode"]),
    iface!("HTMLPictureElement" => "HTMLElement"),
    iface!("HTMLSourceElement" => "HTMLElement"; attrs ["src", "type", "srcset", "sizes", "media", "width", "height"]; ops []),
    iface!("HTMLMediaElement" => "HTMLElement"; attrs ["src", "currentSrc", "networkState", "readyState", "currentTime", "duration", "paused", "ended", "autoplay", "loop", "controls", "muted", "volume", "preload", "crossOrigin", "textTracks"]; ops ["load", "play", "pause", "canPlayType"]),
    iface!("HTMLVideoElement" => "HTMLMediaElement"; attrs ["width", "height", "videoWidth", "videoHeight", "poster", "playsInline"]; ops []),
    iface!("HTMLAudioElement" => "HTMLMediaElement"),
    iface!("HTMLTrackElement" => "HTMLElement"; attrs ["kind", "src", "srclang", "label", "default", "readyState", "track"]; ops []),
    iface!("TextTrack" => "EventTarget"; attrs ["kind", "label", "language", "id", "mode", "cues", "activeCues"]; ops ["addCue", "removeCue"]),
    iface!("TextTrackList" => "EventTarget"; attrs ["length"]; ops ["item", "getTrackById"]),
    iface!("TextTrackCue" => "EventTarget"; attrs ["track", "id", "startTime", "endTime", "pauseOnExit"]; ops []),
    iface!("TimeRanges"; attrs ["length"]; ops ["start", "end"]),
    WebIdlInterface {
        name: "HTMLIFrameElement",
        parent: Some("HTMLElement"),
        attributes: &[
            "src",
            "srcdoc",
            "name",
            "sandbox",
            "allow",
            "width",
            "height",
            "contentDocument",
            "contentWindow",
        ],
        operations: &[],
    },
    iface!("HTMLEmbedElement" => "HTMLElement"; attrs ["src", "type", "width", "height"]; ops []),
    iface!("HTMLObjectElement" => "HTMLElement"; attrs ["data", "type", "name", "useMap", "width", "height", "contentDocument", "contentWindow"]; ops ["checkValidity", "reportValidity", "setCustomValidity"]),
    iface!("HTMLParamElement" => "HTMLElement"; attrs ["name", "value"]; ops []),
    iface!("HTMLCanvasElement" => "HTMLElement"; attrs ["width", "height"]; ops ["getContext", "toDataURL", "toBlob", "transferControlToOffscreen"]),
    iface!("CanvasRenderingContext2D"; attrs ["canvas", "globalAlpha", "globalCompositeOperation", "fillStyle", "strokeStyle", "lineWidth", "font", "textAlign", "textBaseline"]; ops ["save", "restore", "scale", "rotate", "translate", "transform", "setTransform", "resetTransform", "clearRect", "fillRect", "strokeRect", "beginPath", "closePath", "moveTo", "lineTo", "bezierCurveTo", "quadraticCurveTo", "arc", "rect", "fill", "stroke", "clip", "drawImage", "fillText", "strokeText", "measureText", "getImageData", "putImageData", "createImageData", "createLinearGradient", "createRadialGradient", "createPattern"]),
    iface!("CanvasGradient"; attrs []; ops ["addColorStop"]),
    iface!("CanvasPattern"; attrs []; ops ["setTransform"]),
    iface!("ImageData"; attrs ["width", "height", "data", "colorSpace"]; ops []),
    iface!("TextMetrics"; attrs ["width", "actualBoundingBoxLeft", "actualBoundingBoxRight", "fontBoundingBoxAscent", "fontBoundingBoxDescent", "actualBoundingBoxAscent", "actualBoundingBoxDescent"]; ops []),
    iface!("OffscreenCanvas" => "EventTarget"; attrs ["width", "height"]; ops ["getContext", "convertToBlob", "transferToImageBitmap"]),
    iface!("OffscreenCanvasRenderingContext2D"; attrs ["canvas"]; ops ["commit"]),
    iface!("ImageBitmap"; attrs ["width", "height"]; ops ["close"]),
    iface!("ImageBitmapRenderingContext"; attrs ["canvas"]; ops ["transferFromImageBitmap"]),
    iface!("Path2D"; attrs []; ops ["addPath", "closePath", "moveTo", "lineTo", "bezierCurveTo", "quadraticCurveTo", "arc", "rect"]),
    iface!("HTMLTableElement" => "HTMLElement"; attrs ["caption", "tHead", "tFoot", "rows", "tBodies"]; ops ["createCaption", "deleteCaption", "createTHead", "deleteTHead", "createTFoot", "deleteTFoot", "createTBody", "insertRow", "deleteRow"]),
    iface!("HTMLTableCaptionElement" => "HTMLElement"),
    iface!("HTMLTableColElement" => "HTMLElement"; attrs ["span"]; ops []),
    iface!("HTMLTableSectionElement" => "HTMLElement"; attrs ["rows"]; ops ["insertRow", "deleteRow"]),
    iface!("HTMLTableRowElement" => "HTMLElement"; attrs ["rowIndex", "sectionRowIndex", "cells"]; ops ["insertCell", "deleteCell"]),
    iface!("HTMLTableCellElement" => "HTMLElement"; attrs ["colSpan", "rowSpan", "headers", "cellIndex", "scope", "abbr"]; ops []),
    iface!("HTMLFormElement" => "HTMLElement"; attrs ["acceptCharset", "action", "autocomplete", "enctype", "encoding", "method", "name", "noValidate", "target", "elements", "length"]; ops ["submit", "requestSubmit", "reset", "checkValidity", "reportValidity"]),
    iface!("HTMLLabelElement" => "HTMLElement"; attrs ["control", "form", "htmlFor"]; ops []),
    iface!("HTMLInputElement" => "HTMLElement"; attrs ["accept", "alt", "autocomplete", "checked", "defaultChecked", "defaultValue", "disabled", "files", "form", "formAction", "formEnctype", "formMethod", "formNoValidate", "formTarget", "height", "indeterminate", "list", "max", "maxLength", "min", "minLength", "multiple", "name", "pattern", "placeholder", "readOnly", "required", "size", "src", "step", "type", "value", "valueAsDate", "valueAsNumber", "width", "willValidate", "validity", "validationMessage"]; ops ["stepUp", "stepDown", "checkValidity", "reportValidity", "setCustomValidity", "select", "setRangeText", "setSelectionRange", "showPicker"]),
    iface!("HTMLButtonElement" => "HTMLElement"; attrs ["disabled", "form", "formAction", "formEnctype", "formMethod", "formNoValidate", "formTarget", "name", "type", "value", "willValidate", "validity", "validationMessage"]; ops ["checkValidity", "reportValidity", "setCustomValidity"]),
    iface!("HTMLSelectElement" => "HTMLElement"; attrs ["autocomplete", "disabled", "form", "length", "multiple", "name", "required", "selectedIndex", "selectedOptions", "size", "type", "value", "willValidate", "validity", "validationMessage", "options"]; ops ["item", "namedItem", "add", "remove", "checkValidity", "reportValidity", "setCustomValidity", "showPicker"]),
    iface!("HTMLDataListElement" => "HTMLElement"; attrs ["options"]; ops []),
    iface!("HTMLOptGroupElement" => "HTMLElement"; attrs ["disabled", "label"]; ops []),
    iface!("HTMLOptionElement" => "HTMLElement"; attrs ["disabled", "form", "label", "defaultSelected", "selected", "value", "text", "index"]; ops []),
    iface!("HTMLOptionsCollection" => "HTMLCollection"; attrs ["selectedIndex"]; ops ["add", "remove"]),
    iface!("HTMLTextAreaElement" => "HTMLElement"; attrs ["autocomplete", "cols", "disabled", "form", "maxLength", "minLength", "name", "placeholder", "readOnly", "required", "rows", "wrap", "type", "defaultValue", "value", "textLength", "willValidate", "validity", "validationMessage"]; ops ["checkValidity", "reportValidity", "setCustomValidity", "select", "setRangeText", "setSelectionRange"]),
    iface!("HTMLProgressElement" => "HTMLElement"; attrs ["value", "max", "position", "labels"]; ops []),
    iface!("HTMLMeterElement" => "HTMLElement"; attrs ["value", "min", "max", "low", "high", "optimum", "labels"]; ops []),
    iface!("HTMLFieldSetElement" => "HTMLElement"; attrs ["disabled", "form", "name", "type", "elements", "willValidate", "validity", "validationMessage"]; ops ["checkValidity", "reportValidity", "setCustomValidity"]),
    iface!("HTMLLegendElement" => "HTMLElement"; attrs ["form"]; ops []),
    iface!("HTMLOutputElement" => "HTMLElement"; attrs ["htmlFor", "form", "name", "type", "defaultValue", "value", "willValidate", "validity", "validationMessage"]; ops ["checkValidity", "reportValidity", "setCustomValidity"]),
    iface!("HTMLDetailsElement" => "HTMLElement"; attrs ["open", "name"]; ops []),
    iface!("HTMLDialogElement" => "HTMLElement"; attrs ["open", "returnValue"]; ops ["show", "showModal", "close", "requestClose"]),
    iface!("HTMLMenuElement" => "HTMLElement"),
    WebIdlInterface {
        name: "Document",
        parent: Some("Node"),
        attributes: &[
            "title",
            "URL",
            "documentURI",
            "readyState",
            "body",
            "documentElement",
            "head",
            "activeElement",
            "scrollingElement",
            "styleSheets",
            "forms",
            "images",
            "links",
            "scripts",
            "defaultView",
            "location",
            "baseURI",
            "characterSet",
            "contentType",
            "compatMode",
            "visibilityState",
            "hidden",
            "referrer",
            "cookie",
        ],
        operations: &[
            "querySelector",
            "querySelectorAll",
            "elementFromPoint",
            "elementsFromPoint",
            "getElementById",
            "getElementsByTagName",
            "getElementsByClassName",
            "createElement",
            "createElementNS",
            "createTextNode",
            "createDocumentFragment",
            "createRange",
            "createNodeIterator",
            "createTreeWalker",
            "getSelection",
            "hasFocus",
            "write",
            "open",
            "close",
        ],
    },
    iface!("XMLDocument" => "Document"),
    iface!("DOMImplementation"; attrs []; ops ["createDocumentType", "createDocument", "createHTMLDocument", "hasFeature"]),
    iface!("DOMParser"; attrs []; ops ["parseFromString"]),
    iface!("XMLSerializer"; attrs []; ops ["serializeToString"]),
    iface!("Range"; attrs ["startContainer", "startOffset", "endContainer", "endOffset", "collapsed", "commonAncestorContainer"]; ops ["setStart", "setEnd", "collapse", "selectNode", "selectNodeContents", "deleteContents", "extractContents", "cloneContents", "insertNode", "surroundContents", "cloneRange", "detach", "isPointInRange", "comparePoint", "intersectsNode", "getBoundingClientRect", "getClientRects", "toString"]),
    iface!("StaticRange"; attrs ["startContainer", "startOffset", "endContainer", "endOffset", "collapsed"]; ops []),
    iface!("Selection"; attrs ["anchorNode", "anchorOffset", "focusNode", "focusOffset", "isCollapsed", "rangeCount", "type", "direction"]; ops ["getRangeAt", "addRange", "removeRange", "removeAllRanges", "empty", "collapse", "setPosition", "collapseToStart", "collapseToEnd", "extend", "selectAllChildren", "deleteFromDocument", "containsNode", "toString"]),
    iface!("NodeIterator"; attrs ["root", "referenceNode", "pointerBeforeReferenceNode", "whatToShow", "filter"]; ops ["nextNode", "previousNode", "detach"]),
    iface!("TreeWalker"; attrs ["root", "whatToShow", "filter", "currentNode"]; ops ["parentNode", "firstChild", "lastChild", "previousSibling", "nextSibling", "previousNode", "nextNode"]),
    iface!("MutationObserver"; attrs []; ops ["observe", "disconnect", "takeRecords"]),
    iface!("MutationRecord"; attrs ["type", "target", "addedNodes", "removedNodes", "previousSibling", "nextSibling", "attributeName", "attributeNamespace", "oldValue"]; ops []),
    WebIdlInterface {
        name: "DOMTokenList",
        parent: None,
        attributes: &["length", "value"],
        operations: &["item", "contains", "toString"],
    },
    WebIdlInterface {
        name: "DOMStringMap",
        parent: None,
        attributes: &[],
        operations: &[],
    },
    iface!("FormData"; attrs []; ops ["append", "delete", "get", "getAll", "has", "set", "entries", "keys", "values", "forEach"]),
    iface!("ValidityState"; attrs ["valueMissing", "typeMismatch", "patternMismatch", "tooLong", "tooShort", "rangeUnderflow", "rangeOverflow", "stepMismatch", "badInput", "customError", "valid"]; ops []),
    iface!("FileList"; attrs ["length"]; ops ["item"]),
    iface!("DataTransfer"; attrs ["dropEffect", "effectAllowed", "items", "types", "files"]; ops ["getData", "setData", "clearData", "setDragImage"]),
    iface!("DataTransferItem"; attrs ["kind", "type"]; ops ["getAsString", "getAsFile"]),
    iface!("DataTransferItemList"; attrs ["length"]; ops ["item", "add", "remove", "clear"]),
    WebIdlInterface {
        name: "DOMRectReadOnly",
        parent: None,
        attributes: &[
            "x", "y", "width", "height", "top", "right", "bottom", "left",
        ],
        operations: &["toJSON"],
    },
    WebIdlInterface {
        name: "DOMRect",
        parent: Some("DOMRectReadOnly"),
        attributes: &["x", "y", "width", "height"],
        operations: &[],
    },
    WebIdlInterface {
        name: "DOMRectList",
        parent: None,
        attributes: &["length"],
        operations: &["item"],
    },
    iface!("DOMPointReadOnly"; attrs ["x", "y", "z", "w"]; ops ["matrixTransform", "toJSON"]),
    iface!("DOMPoint" => "DOMPointReadOnly"; attrs ["x", "y", "z", "w"]; ops ["fromPoint"]),
    iface!("DOMQuad"; attrs ["p1", "p2", "p3", "p4"]; ops ["getBounds", "toJSON", "fromRect", "fromQuad"]),
    iface!("DOMMatrixReadOnly"; attrs ["a", "b", "c", "d", "e", "f", "m11", "m12", "m13", "m14", "m21", "m22", "m23", "m24", "m31", "m32", "m33", "m34", "m41", "m42", "m43", "m44", "is2D", "isIdentity"]; ops ["translate", "scale", "rotate", "skewX", "skewY", "multiply", "flipX", "flipY", "inverse", "transformPoint", "toFloat32Array", "toFloat64Array", "toString", "toJSON"]),
    iface!("DOMMatrix" => "DOMMatrixReadOnly"; attrs ["a", "b", "c", "d", "e", "f", "m11", "m12", "m13", "m14", "m21", "m22", "m23", "m24", "m31", "m32", "m33", "m34", "m41", "m42", "m43", "m44"]; ops ["multiplySelf", "preMultiplySelf", "translateSelf", "scaleSelf", "rotateSelf", "skewXSelf", "skewYSelf", "invertSelf", "setMatrixValue", "fromMatrix", "fromFloat32Array", "fromFloat64Array"]),
    iface!("GeometryUtils"; attrs []; ops ["getBoxQuads", "convertQuadFromNode", "convertRectFromNode", "convertPointFromNode"]),
    WebIdlInterface {
        name: "CSSStyleDeclaration",
        parent: None,
        attributes: &["length"],
        operations: &["item", "getPropertyValue"],
    },
    WebIdlInterface {
        name: "CSSRule",
        parent: None,
        attributes: &["cssText"],
        operations: &[],
    },
    WebIdlInterface {
        name: "CSSStyleRule",
        parent: Some("CSSRule"),
        attributes: &["selectorText", "style"],
        operations: &[],
    },
    WebIdlInterface {
        name: "CSSRuleList",
        parent: None,
        attributes: &["length"],
        operations: &["item"],
    },
    WebIdlInterface {
        name: "StyleSheet",
        parent: None,
        attributes: &["disabled", "href", "ownerNode"],
        operations: &[],
    },
    WebIdlInterface {
        name: "CSSStyleSheet",
        parent: Some("StyleSheet"),
        attributes: &["cssRules"],
        operations: &[],
    },
    WebIdlInterface {
        name: "StyleSheetList",
        parent: None,
        attributes: &["length"],
        operations: &["item"],
    },
    iface!("CSS"; attrs []; ops ["supports", "escape", "registerProperty"]),
    iface!("CSSImportRule" => "CSSRule"; attrs ["href", "media", "styleSheet", "layerName", "supportsText"]; ops []),
    iface!("CSSGroupingRule" => "CSSRule"; attrs ["cssRules"]; ops ["insertRule", "deleteRule"]),
    iface!("CSSMediaRule" => "CSSGroupingRule"; attrs ["media"]; ops []),
    iface!("CSSSupportsRule" => "CSSGroupingRule"; attrs ["conditionText"]; ops []),
    iface!("CSSLayerBlockRule" => "CSSGroupingRule"; attrs ["name"]; ops []),
    iface!("CSSLayerStatementRule" => "CSSRule"; attrs ["nameList"]; ops []),
    iface!("CSSNamespaceRule" => "CSSRule"; attrs ["namespaceURI", "prefix"]; ops []),
    iface!("CSSPageRule" => "CSSGroupingRule"; attrs ["selectorText", "style"]; ops []),
    iface!("CSSKeyframesRule" => "CSSRule"; attrs ["name", "cssRules"]; ops ["appendRule", "deleteRule", "findRule"]),
    iface!("CSSKeyframeRule" => "CSSRule"; attrs ["keyText", "style"]; ops []),
    iface!("CSSFontFaceRule" => "CSSRule"; attrs ["style"]; ops []),
    iface!("CSSCounterStyleRule" => "CSSRule"; attrs ["name", "system", "symbols", "additiveSymbols", "negative", "prefix", "suffix", "range", "pad", "speakAs", "fallback"]; ops []),
    iface!("CSSContainerRule" => "CSSGroupingRule"; attrs ["containerName", "containerQuery"]; ops []),
    iface!("MediaList"; attrs ["mediaText", "length"]; ops ["item", "appendMedium", "deleteMedium"]),
    iface!("CSSStyleValue"; attrs []; ops ["parse", "parseAll"]),
    iface!("CSSUnparsedValue" => "CSSStyleValue"; attrs ["length"]; ops ["entries", "keys", "values", "forEach"]),
    iface!("CSSKeywordValue" => "CSSStyleValue"; attrs ["value"]; ops []),
    iface!("CSSNumericValue" => "CSSStyleValue"; attrs []; ops ["parse", "add", "sub", "mul", "div", "min", "max", "equals", "to", "toSum", "type"]),
    iface!("CSSUnitValue" => "CSSNumericValue"; attrs ["value", "unit"]; ops []),
    iface!("CSSMathValue" => "CSSNumericValue"; attrs ["operator"]; ops []),
    iface!("CSSMathSum" => "CSSMathValue"; attrs ["values"]; ops []),
    iface!("CSSMathProduct" => "CSSMathValue"; attrs ["values"]; ops []),
    iface!("CSSMathNegate" => "CSSMathValue"; attrs ["value"]; ops []),
    iface!("CSSMathInvert" => "CSSMathValue"; attrs ["value"]; ops []),
    iface!("CSSMathMin" => "CSSMathValue"; attrs ["values"]; ops []),
    iface!("CSSMathMax" => "CSSMathValue"; attrs ["values"]; ops []),
    iface!("CSSMathClamp" => "CSSMathValue"; attrs ["lower", "value", "upper"]; ops []),
    iface!("CSSTransformValue" => "CSSStyleValue"; attrs ["length", "is2D"]; ops ["toMatrix"]),
    iface!("CSSTranslate" => "CSSTransformComponent"),
    iface!("CSSRotate" => "CSSTransformComponent"),
    iface!("CSSScale" => "CSSTransformComponent"),
    iface!("CSSSkew" => "CSSTransformComponent"),
    iface!("CSSSkewX" => "CSSTransformComponent"),
    iface!("CSSSkewY" => "CSSTransformComponent"),
    iface!("CSSPerspective" => "CSSTransformComponent"),
    iface!("CSSMatrixComponent" => "CSSTransformComponent"),
    iface!("CSSTransformComponent"; attrs ["is2D"]; ops ["toMatrix"]),
    iface!("CSSPositionValue" => "CSSStyleValue"; attrs ["x", "y"]; ops []),
    iface!("CSSImageValue" => "CSSStyleValue"),
    iface!("StylePropertyMap"; attrs ["size"]; ops ["get", "getAll", "has", "set", "append", "delete", "clear", "entries", "keys", "values", "forEach"]),
    iface!("StylePropertyMapReadOnly"; attrs ["size"]; ops ["get", "getAll", "has", "entries", "keys", "values", "forEach"]),
    iface!("Screen" => "EventTarget"; attrs ["availWidth", "availHeight", "width", "height", "colorDepth", "pixelDepth", "orientation"]; ops []),
    iface!("ScreenOrientation" => "EventTarget"; attrs ["type", "angle"]; ops ["lock", "unlock"]),
    iface!("VisualViewport" => "EventTarget"; attrs ["offsetLeft", "offsetTop", "pageLeft", "pageTop", "width", "height", "scale"]; ops []),
    iface!("Navigator"; attrs ["userAgent", "language", "languages", "onLine", "cookieEnabled", "hardwareConcurrency", "maxTouchPoints", "permissions", "storage", "clipboard", "serviceWorker", "geolocation", "mediaDevices"]; ops ["sendBeacon", "share", "canShare", "vibrate", "getBattery", "registerProtocolHandler"]),
    iface!("Permissions"; attrs []; ops ["query"]),
    iface!("PermissionStatus" => "EventTarget"; attrs ["state", "onchange"]; ops []),
    iface!("Location"; attrs ["href", "origin", "protocol", "host", "hostname", "port", "pathname", "search", "hash"]; ops ["assign", "replace", "reload", "toString"]),
    iface!("History"; attrs ["length", "scrollRestoration", "state"]; ops ["go", "back", "forward", "pushState", "replaceState"]),
    iface!("BarProp"; attrs ["visible"]; ops []),
    iface!("Performance" => "EventTarget"; attrs ["timeOrigin", "navigation", "timing", "memory"]; ops ["now", "mark", "measure", "clearMarks", "clearMeasures", "getEntries", "getEntriesByName", "getEntriesByType", "toJSON"]),
    iface!("PerformanceEntry"; attrs ["name", "entryType", "startTime", "duration"]; ops ["toJSON"]),
    iface!("PerformanceMark" => "PerformanceEntry"; attrs ["detail"]; ops []),
    iface!("PerformanceMeasure" => "PerformanceEntry"; attrs ["detail"]; ops []),
    iface!("PerformanceObserver"; attrs []; ops ["observe", "disconnect", "takeRecords"]),
    iface!("PerformanceObserverEntryList"; attrs []; ops ["getEntries", "getEntriesByName", "getEntriesByType"]),
    iface!("PerformanceResourceTiming" => "PerformanceEntry"),
    iface!("PerformanceNavigationTiming" => "PerformanceResourceTiming"),
    iface!("PerformancePaintTiming" => "PerformanceEntry"),
    iface!("Event"; attrs ["type", "target", "currentTarget", "eventPhase", "bubbles", "cancelable", "defaultPrevented", "composed", "timeStamp", "isTrusted"]; ops ["stopPropagation", "stopImmediatePropagation", "preventDefault", "composedPath"]),
    iface!("CustomEvent" => "Event"; attrs ["detail"]; ops ["initCustomEvent"]),
    iface!("UIEvent" => "Event"; attrs ["view", "detail"]; ops []),
    iface!("MouseEvent" => "UIEvent"; attrs ["screenX", "screenY", "clientX", "clientY", "ctrlKey", "shiftKey", "altKey", "metaKey", "button", "buttons", "relatedTarget"]; ops ["getModifierState"]),
    iface!("PointerEvent" => "MouseEvent"; attrs ["pointerId", "width", "height", "pressure", "tangentialPressure", "tiltX", "tiltY", "twist", "pointerType", "isPrimary"]; ops []),
    iface!("WheelEvent" => "MouseEvent"; attrs ["deltaX", "deltaY", "deltaZ", "deltaMode"]; ops []),
    iface!("KeyboardEvent" => "UIEvent"; attrs ["key", "code", "location", "ctrlKey", "shiftKey", "altKey", "metaKey", "repeat", "isComposing"]; ops ["getModifierState"]),
    iface!("InputEvent" => "UIEvent"; attrs ["data", "isComposing", "inputType", "dataTransfer"]; ops ["getTargetRanges"]),
    iface!("FocusEvent" => "UIEvent"; attrs ["relatedTarget"]; ops []),
    iface!("CompositionEvent" => "UIEvent"; attrs ["data"]; ops []),
    iface!("DragEvent" => "MouseEvent"; attrs ["dataTransfer"]; ops []),
    iface!("SubmitEvent" => "Event"; attrs ["submitter"]; ops []),
    iface!("ErrorEvent" => "Event"; attrs ["message", "filename", "lineno", "colno", "error"]; ops []),
    iface!("PromiseRejectionEvent" => "Event"; attrs ["promise", "reason"]; ops []),
    iface!("MessageEvent" => "Event"; attrs ["data", "origin", "lastEventId", "source", "ports"]; ops []),
    iface!("ProgressEvent" => "Event"; attrs ["lengthComputable", "loaded", "total"]; ops []),
    iface!("BeforeUnloadEvent" => "Event"; attrs ["returnValue"]; ops []),
    iface!("HashChangeEvent" => "Event"; attrs ["oldURL", "newURL"]; ops []),
    iface!("PageTransitionEvent" => "Event"; attrs ["persisted"]; ops []),
    iface!("PopStateEvent" => "Event"; attrs ["state"]; ops []),
    iface!("StorageEvent" => "Event"; attrs ["key", "oldValue", "newValue", "url", "storageArea"]; ops []),
    iface!("AnimationEvent" => "Event"; attrs ["animationName", "elapsedTime", "pseudoElement"]; ops []),
    iface!("TransitionEvent" => "Event"; attrs ["propertyName", "elapsedTime", "pseudoElement"]; ops []),
    iface!("ClipboardEvent" => "Event"; attrs ["clipboardData"]; ops []),
    iface!("Touch"; attrs ["identifier", "target", "screenX", "screenY", "clientX", "clientY", "pageX", "pageY"]; ops []),
    iface!("TouchList"; attrs ["length"]; ops ["item"]),
    iface!("TouchEvent" => "UIEvent"; attrs ["touches", "targetTouches", "changedTouches", "altKey", "metaKey", "ctrlKey", "shiftKey"]; ops []),
    iface!("AbortController"; attrs ["signal"]; ops ["abort"]),
    iface!("AbortSignal" => "EventTarget"; attrs ["aborted", "reason"]; ops ["throwIfAborted", "abort", "timeout", "any"]),
    iface!("Blob"; attrs ["size", "type"]; ops ["arrayBuffer", "bytes", "slice", "stream", "text"]),
    iface!("File" => "Blob"; attrs ["name", "lastModified", "webkitRelativePath"]; ops []),
    iface!("FileReader" => "EventTarget"; attrs ["readyState", "result", "error"]; ops ["abort", "readAsArrayBuffer", "readAsBinaryString", "readAsDataURL", "readAsText"]),
    iface!("URL"; attrs ["href", "origin", "protocol", "username", "password", "host", "hostname", "port", "pathname", "search", "searchParams", "hash"]; ops ["toString", "toJSON", "canParse", "parse", "createObjectURL", "revokeObjectURL"]),
    iface!("URLSearchParams"; attrs ["size"]; ops ["append", "delete", "get", "getAll", "has", "set", "sort", "entries", "keys", "values", "forEach", "toString"]),
    iface!("URLPattern"; attrs ["protocol", "username", "password", "hostname", "port", "pathname", "search", "hash"]; ops ["test", "exec"]),
    iface!("Headers"; attrs []; ops ["append", "delete", "get", "getSetCookie", "has", "set", "entries", "keys", "values", "forEach"]),
    iface!("Request"; attrs ["method", "url", "headers", "destination", "referrer", "referrerPolicy", "mode", "credentials", "cache", "redirect", "integrity", "keepalive", "signal", "body", "bodyUsed"]; ops ["arrayBuffer", "blob", "bytes", "clone", "formData", "json", "text"]),
    iface!("Response"; attrs ["type", "url", "redirected", "status", "ok", "statusText", "headers", "body", "bodyUsed"]; ops ["arrayBuffer", "blob", "bytes", "clone", "formData", "json", "text", "error", "redirect"]),
    iface!("ReadableStream"; attrs ["locked"]; ops ["cancel", "getReader", "pipeThrough", "pipeTo", "tee"]),
    iface!("ReadableStreamDefaultReader"; attrs ["closed"]; ops ["cancel", "read", "releaseLock"]),
    iface!("ReadableStreamBYOBReader"; attrs ["closed"]; ops ["cancel", "read", "releaseLock"]),
    iface!("ReadableStreamDefaultController"; attrs ["desiredSize"]; ops ["close", "enqueue", "error"]),
    iface!("ReadableByteStreamController"; attrs ["byobRequest", "desiredSize"]; ops ["close", "enqueue", "error"]),
    iface!("ReadableStreamBYOBRequest"; attrs ["view"]; ops ["respond", "respondWithNewView"]),
    iface!("WritableStream"; attrs ["locked"]; ops ["abort", "close", "getWriter"]),
    iface!("WritableStreamDefaultWriter"; attrs ["closed", "desiredSize", "ready"]; ops ["abort", "close", "releaseLock", "write"]),
    iface!("WritableStreamDefaultController"; attrs ["signal"]; ops ["error"]),
    iface!("TransformStream"; attrs ["readable", "writable"]; ops []),
    iface!("TransformStreamDefaultController"; attrs ["desiredSize"]; ops ["enqueue", "error", "terminate"]),
    iface!("ByteLengthQueuingStrategy"; attrs ["highWaterMark", "size"]; ops []),
    iface!("CountQueuingStrategy"; attrs ["highWaterMark", "size"]; ops []),
    iface!("TextEncoder"; attrs ["encoding"]; ops ["encode", "encodeInto"]),
    iface!("TextDecoder"; attrs ["encoding", "fatal", "ignoreBOM"]; ops ["decode"]),
    iface!("TextEncoderStream"; attrs ["encoding", "readable", "writable"]; ops []),
    iface!("TextDecoderStream"; attrs ["encoding", "fatal", "ignoreBOM", "readable", "writable"]; ops []),
    iface!("CompressionStream"; attrs ["readable", "writable"]; ops []),
    iface!("DecompressionStream"; attrs ["readable", "writable"]; ops []),
    iface!("Storage"; attrs ["length"]; ops ["key", "getItem", "setItem", "removeItem", "clear"]),
    iface!("StorageManager"; attrs []; ops ["estimate", "persist", "persisted", "getDirectory"]),
    iface!("Cache"; attrs []; ops ["match", "matchAll", "add", "addAll", "put", "delete", "keys"]),
    iface!("CacheStorage"; attrs []; ops ["match", "has", "open", "delete", "keys"]),
    iface!("CookieStore" => "EventTarget"; attrs []; ops ["get", "getAll", "set", "delete"]),
    iface!("CookieChangeEvent" => "Event"; attrs ["changed", "deleted"]; ops []),
    iface!("BroadcastChannel" => "EventTarget"; attrs ["name"]; ops ["postMessage", "close"]),
    iface!("MessageChannel"; attrs ["port1", "port2"]; ops []),
    iface!("MessagePort" => "EventTarget"; attrs []; ops ["postMessage", "start", "close"]),
    iface!("Worker" => "EventTarget"; attrs []; ops ["postMessage", "terminate"]),
    iface!("SharedWorker" => "EventTarget"; attrs ["port"]; ops []),
    iface!("WorkerGlobalScope" => "EventTarget"; attrs ["self", "location", "navigator"]; ops ["importScripts", "atob", "btoa", "setTimeout", "clearTimeout", "fetch"]),
    iface!("DedicatedWorkerGlobalScope" => "WorkerGlobalScope"; attrs ["name"]; ops ["postMessage", "close"]),
    iface!("SharedWorkerGlobalScope" => "WorkerGlobalScope"; attrs ["name", "applicationCache"]; ops ["close"]),
    iface!("ServiceWorker" => "EventTarget"; attrs ["scriptURL", "state"]; ops ["postMessage"]),
    iface!("ServiceWorkerRegistration" => "EventTarget"; attrs ["installing", "waiting", "active", "scope", "navigationPreload"]; ops ["update", "unregister", "showNotification", "getNotifications"]),
    iface!("ServiceWorkerContainer" => "EventTarget"; attrs ["controller", "ready"]; ops ["register", "getRegistration", "getRegistrations", "startMessages"]),
    iface!("NavigationPreloadManager"; attrs []; ops ["enable", "disable", "setHeaderValue", "getState"]),
    iface!("WebSocket" => "EventTarget"; attrs ["url", "readyState", "bufferedAmount", "extensions", "protocol", "binaryType"]; ops ["send", "close"]),
    iface!("CloseEvent" => "Event"; attrs ["wasClean", "code", "reason"]; ops []),
    iface!("EventSource" => "EventTarget"; attrs ["url", "withCredentials", "readyState"]; ops ["close"]),
    iface!("XMLHttpRequest" => "EventTarget"; attrs ["readyState", "timeout", "withCredentials", "upload", "responseURL", "status", "statusText", "responseType", "response", "responseText", "responseXML"]; ops ["open", "setRequestHeader", "send", "abort", "getResponseHeader", "getAllResponseHeaders", "overrideMimeType"]),
    iface!("XMLHttpRequestUpload" => "EventTarget"),
    iface!("XMLHttpRequestEventTarget" => "EventTarget"),
    iface!("WebGLObject"),
    iface!("WebGLBuffer" => "WebGLObject"),
    iface!("WebGLFramebuffer" => "WebGLObject"),
    iface!("WebGLProgram" => "WebGLObject"),
    iface!("WebGLRenderbuffer" => "WebGLObject"),
    iface!("WebGLShader" => "WebGLObject"),
    iface!("WebGLTexture" => "WebGLObject"),
    iface!("WebGLUniformLocation"),
    iface!("WebGLActiveInfo"),
    iface!("WebGLShaderPrecisionFormat"),
    iface!("WebGLRenderingContext"; attrs ["canvas", "drawingBufferWidth", "drawingBufferHeight"]; ops ["getContextAttributes", "isContextLost", "getSupportedExtensions", "getExtension", "activeTexture", "attachShader", "bindAttribLocation", "bindBuffer", "bindFramebuffer", "bindRenderbuffer", "bindTexture", "blendColor", "blendEquation", "blendFunc", "bufferData", "clear", "clearColor", "compileShader", "createBuffer", "createFramebuffer", "createProgram", "createRenderbuffer", "createShader", "createTexture", "deleteBuffer", "deleteFramebuffer", "deleteProgram", "deleteRenderbuffer", "deleteShader", "deleteTexture", "drawArrays", "drawElements", "enable", "disable", "finish", "flush", "getParameter", "linkProgram", "shaderSource", "texImage2D", "uniform1f", "uniform1i", "useProgram", "viewport"]),
    iface!("WebGL2RenderingContext" => "WebGLRenderingContext"),
    iface!("WebGLQuery" => "WebGLObject"),
    iface!("WebGLSampler" => "WebGLObject"),
    iface!("WebGLSync" => "WebGLObject"),
    iface!("WebGLTransformFeedback" => "WebGLObject"),
    iface!("WebGLVertexArrayObject" => "WebGLObject"),
    iface!("GPU"; attrs []; ops ["requestAdapter", "getPreferredCanvasFormat"]),
    iface!("GPUAdapter"; attrs ["features", "limits", "isFallbackAdapter", "info"]; ops ["requestDevice"]),
    iface!("GPUDevice" => "EventTarget"; attrs ["features", "limits", "queue", "lost"]; ops ["createBuffer", "createTexture", "createSampler", "createBindGroupLayout", "createPipelineLayout", "createBindGroup", "createShaderModule", "createComputePipeline", "createRenderPipeline", "createCommandEncoder", "createRenderBundleEncoder", "createQuerySet", "destroy"]),
    iface!("GPUQueue"; attrs []; ops ["submit", "writeBuffer", "writeTexture", "copyExternalImageToTexture", "onSubmittedWorkDone"]),
    iface!("GPUBuffer"; attrs ["size", "usage", "mapState"]; ops ["mapAsync", "getMappedRange", "unmap", "destroy"]),
    iface!("GPUTexture"; attrs ["width", "height", "depthOrArrayLayers", "mipLevelCount", "sampleCount", "dimension", "format", "usage"]; ops ["createView", "destroy"]),
    iface!("GPUSampler"),
    iface!("GPUBindGroupLayout"),
    iface!("GPUPipelineLayout"),
    iface!("GPUBindGroup"),
    iface!("GPUShaderModule"; attrs []; ops ["getCompilationInfo"]),
    iface!("GPUComputePipeline"; attrs []; ops ["getBindGroupLayout"]),
    iface!("GPURenderPipeline"; attrs []; ops ["getBindGroupLayout"]),
    iface!("GPUCommandEncoder"; attrs []; ops ["beginRenderPass", "beginComputePass", "copyBufferToBuffer", "copyBufferToTexture", "copyTextureToBuffer", "copyTextureToTexture", "clearBuffer", "resolveQuerySet", "finish"]),
    iface!("GPUCommandBuffer"),
    iface!("GPURenderPassEncoder"; attrs []; ops ["setPipeline", "setIndexBuffer", "setVertexBuffer", "draw", "drawIndexed", "end"]),
    iface!("GPUComputePassEncoder"; attrs []; ops ["setPipeline", "dispatchWorkgroups", "end"]),
    iface!("GPUCanvasContext"; attrs ["canvas"]; ops ["configure", "unconfigure", "getCurrentTexture"]),
    iface!("MediaDevices" => "EventTarget"; attrs []; ops ["enumerateDevices", "getSupportedConstraints", "getUserMedia", "getDisplayMedia"]),
    iface!("MediaDeviceInfo"; attrs ["deviceId", "kind", "label", "groupId"]; ops ["toJSON"]),
    iface!("InputDeviceInfo" => "MediaDeviceInfo"; attrs []; ops ["getCapabilities"]),
    iface!("MediaStream" => "EventTarget"; attrs ["id", "active"]; ops ["getAudioTracks", "getVideoTracks", "getTracks", "getTrackById", "addTrack", "removeTrack", "clone"]),
    iface!("MediaStreamTrack" => "EventTarget"; attrs ["kind", "id", "label", "enabled", "muted", "readyState", "contentHint"]; ops ["clone", "stop", "getCapabilities", "getConstraints", "getSettings", "applyConstraints"]),
    iface!("MediaStreamTrackEvent" => "Event"; attrs ["track"]; ops []),
    iface!("MediaRecorder" => "EventTarget"; attrs ["stream", "mimeType", "state", "videoBitsPerSecond", "audioBitsPerSecond"]; ops ["start", "stop", "pause", "resume", "requestData", "isTypeSupported"]),
    iface!("BlobEvent" => "Event"; attrs ["data", "timecode"]; ops []),
    iface!("RTCPeerConnection" => "EventTarget"; attrs ["localDescription", "currentLocalDescription", "pendingLocalDescription", "remoteDescription", "currentRemoteDescription", "pendingRemoteDescription", "signalingState", "iceGatheringState", "iceConnectionState", "connectionState"]; ops ["createOffer", "createAnswer", "setLocalDescription", "setRemoteDescription", "addIceCandidate", "getConfiguration", "setConfiguration", "close", "createDataChannel", "addTrack", "removeTrack", "getSenders", "getReceivers", "getTransceivers", "getStats"]),
    iface!("RTCSessionDescription"; attrs ["type", "sdp"]; ops ["toJSON"]),
    iface!("RTCIceCandidate"; attrs ["candidate", "sdpMid", "sdpMLineIndex", "usernameFragment"]; ops ["toJSON"]),
    iface!("RTCDataChannel" => "EventTarget"; attrs ["label", "ordered", "maxPacketLifeTime", "maxRetransmits", "protocol", "negotiated", "id", "readyState", "bufferedAmount", "bufferedAmountLowThreshold", "binaryType"]; ops ["send", "close"]),
    iface!("RTCDataChannelEvent" => "Event"; attrs ["channel"]; ops []),
    iface!("RTCPeerConnectionIceEvent" => "Event"; attrs ["candidate", "url"]; ops []),
    iface!("RTCRtpSender"; attrs ["track", "transport", "rtcpTransport", "dtmf"]; ops ["replaceTrack", "setParameters", "getParameters", "getStats", "setStreams"]),
    iface!("RTCRtpReceiver"; attrs ["track", "transport", "rtcpTransport"]; ops ["getParameters", "getContributingSources", "getSynchronizationSources", "getStats"]),
    iface!("RTCRtpTransceiver"; attrs ["mid", "sender", "receiver", "stopped", "direction", "currentDirection"]; ops ["stop", "setCodecPreferences"]),
    iface!("IntersectionObserver"; attrs ["root", "rootMargin", "thresholds", "scrollMargin", "delay", "trackVisibility"]; ops ["observe", "unobserve", "disconnect", "takeRecords"]),
    iface!("IntersectionObserverEntry"; attrs ["time", "rootBounds", "boundingClientRect", "intersectionRect", "isIntersecting", "intersectionRatio", "target"]; ops []),
    iface!("ResizeObserver"; attrs []; ops ["observe", "unobserve", "disconnect"]),
    iface!("ResizeObserverEntry"; attrs ["target", "contentRect", "borderBoxSize", "contentBoxSize", "devicePixelContentBoxSize"]; ops []),
    iface!("ResizeObserverSize"; attrs ["inlineSize", "blockSize"]; ops []),
    iface!("ReportingObserver"; attrs []; ops ["observe", "disconnect", "takeRecords"]),
    iface!("Report"; attrs ["type", "url", "body"]; ops ["toJSON"]),
    iface!("Animation" => "EventTarget"; attrs ["id", "effect", "timeline", "startTime", "currentTime", "playbackRate", "playState", "replaceState", "pending", "ready", "finished"]; ops ["cancel", "finish", "play", "pause", "reverse", "updatePlaybackRate", "persist", "commitStyles"]),
    iface!("AnimationEffect"; attrs []; ops ["getTiming", "getComputedTiming", "updateTiming"]),
    iface!("KeyframeEffect" => "AnimationEffect"; attrs ["target", "pseudoElement", "composite", "iterationComposite"]; ops ["getKeyframes", "setKeyframes"]),
    iface!("DocumentTimeline"; attrs ["currentTime"]; ops []),
    iface!("AnimationTimeline"; attrs ["currentTime"]; ops []),
    iface!("Clipboard" => "EventTarget"; attrs []; ops ["read", "readText", "write", "writeText"]),
    iface!("ClipboardItem"; attrs ["types", "presentationStyle"]; ops ["getType", "supports"]),
    iface!("Notification" => "EventTarget"; attrs ["permission", "maxActions", "title", "dir", "lang", "body", "tag", "image", "icon", "badge", "timestamp", "renotify", "silent", "requireInteraction", "data", "actions"]; ops ["requestPermission", "close"]),
    iface!("Geolocation"; attrs []; ops ["getCurrentPosition", "watchPosition", "clearWatch"]),
    iface!("GeolocationPosition"; attrs ["coords", "timestamp"]; ops []),
    iface!("GeolocationCoordinates"; attrs ["latitude", "longitude", "altitude", "accuracy", "altitudeAccuracy", "heading", "speed"]; ops []),
    iface!("GeolocationPositionError"; attrs ["code", "message"]; ops []),
    iface!("Credential"; attrs ["id", "type"]; ops []),
    iface!("CredentialsContainer"; attrs []; ops ["get", "store", "create", "preventSilentAccess"]),
    iface!("PasswordCredential" => "Credential"; attrs ["password", "name", "iconURL"]; ops []),
    iface!("FederatedCredential" => "Credential"; attrs ["provider", "protocol", "name", "iconURL"]; ops []),
    iface!("PublicKeyCredential" => "Credential"; attrs ["rawId", "response", "authenticatorAttachment"]; ops ["getClientExtensionResults", "isUserVerifyingPlatformAuthenticatorAvailable", "isConditionalMediationAvailable"]),
    iface!("Crypto"; attrs ["subtle"]; ops ["getRandomValues", "randomUUID"]),
    iface!("SubtleCrypto"; attrs []; ops ["encrypt", "decrypt", "sign", "verify", "digest", "generateKey", "deriveKey", "deriveBits", "importKey", "exportKey", "wrapKey", "unwrapKey"]),
    iface!("CryptoKey"; attrs ["type", "extractable", "algorithm", "usages"]; ops []),
    iface!("PaymentRequest" => "EventTarget"; attrs ["id", "shippingAddress", "shippingOption", "shippingType"]; ops ["show", "abort", "canMakePayment", "hasEnrolledInstrument"]),
    iface!("PaymentResponse" => "EventTarget"; attrs ["requestId", "methodName", "details", "shippingAddress", "shippingOption", "payerName", "payerEmail", "payerPhone"]; ops ["complete", "retry", "toJSON"]),
    iface!("IDBFactory"; attrs []; ops ["open", "deleteDatabase", "databases", "cmp"]),
    iface!("IDBRequest" => "EventTarget"; attrs ["result", "error", "source", "transaction", "readyState"]; ops []),
    iface!("IDBOpenDBRequest" => "IDBRequest"),
    iface!("IDBDatabase" => "EventTarget"; attrs ["name", "version", "objectStoreNames"]; ops ["createObjectStore", "deleteObjectStore", "transaction", "close"]),
    iface!("IDBObjectStore"; attrs ["name", "keyPath", "indexNames", "transaction", "autoIncrement"]; ops ["put", "add", "delete", "clear", "get", "getKey", "getAll", "getAllKeys", "count", "openCursor", "openKeyCursor", "index", "createIndex", "deleteIndex"]),
    iface!("IDBIndex"; attrs ["name", "objectStore", "keyPath", "multiEntry", "unique"]; ops ["get", "getKey", "getAll", "getAllKeys", "count", "openCursor", "openKeyCursor"]),
    iface!("IDBKeyRange"; attrs ["lower", "upper", "lowerOpen", "upperOpen"]; ops ["only", "lowerBound", "upperBound", "bound", "includes"]),
    iface!("IDBCursor"; attrs ["source", "direction", "key", "primaryKey", "request"]; ops ["advance", "continue", "continuePrimaryKey"]),
    iface!("IDBCursorWithValue" => "IDBCursor"; attrs ["value"]; ops ["update", "delete"]),
    iface!("IDBTransaction" => "EventTarget"; attrs ["objectStoreNames", "mode", "durability", "db", "error"]; ops ["objectStore", "commit", "abort"]),
    iface!("IDBVersionChangeEvent" => "Event"; attrs ["oldVersion", "newVersion"]; ops []),
];

fn generated_webidl_bootstrap() -> String {
    let mut out = String::from(WEBIDL_BINDING_HELPERS);
    for interface in WEBIDL_INTERFACES {
        out.push_str("\n  defineInterface(");
        push_js_string(&mut out, interface.name);
        out.push_str(", ");
        match interface.parent {
            Some(parent) => push_js_string(&mut out, parent),
            None => out.push_str("null"),
        }
        out.push_str(", ");
        push_js_string_array(&mut out, interface.attributes);
        out.push_str(", ");
        push_js_string_array(&mut out, interface.operations);
        out.push_str(");\n");
    }
    out.push_str(WEBIDL_BINDING_FOOTER);
    out
}

fn push_js_string_array(out: &mut String, values: &[&str]) {
    out.push('[');
    for (idx, value) in values.iter().enumerate() {
        if idx > 0 {
            out.push_str(", ");
        }
        push_js_string(out, value);
    }
    out.push(']');
}

fn push_js_string(out: &mut String, value: &str) {
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(ch),
        }
    }
    out.push('"');
}

const WEBIDL_BINDING_HELPERS: &str = r#"
(() => {
  const constructors = new Map();
  const parents = new Map();
  const children = new Map();

  function unsupportedMember(interfaceName, memberName) {
    return function () {
      throw new TypeError(interfaceName + '.' + memberName + ' is not implemented by Vixen yet');
    };
  }

  function illegalConstructor(interfaceName) {
    return function () {
      throw new TypeError('Illegal constructor: ' + interfaceName);
    };
  }

  function defineGlobalInterface(interfaceName, ctor) {
    if (!Object.prototype.hasOwnProperty.call(globalThis, interfaceName)) {
      Object.defineProperty(globalThis, interfaceName, {
        value: ctor,
        writable: true,
        configurable: true,
      });
    }
  }

  function ensureInterfaceConstructor(interfaceName) {
    let ctor = constructors.get(interfaceName);
    if (!ctor) {
      ctor = illegalConstructor(interfaceName);
      constructors.set(interfaceName, ctor);
    }
    defineGlobalInterface(interfaceName, ctor);
    return ctor;
  }

  function rememberParent(interfaceName, parentName) {
    parents.set(interfaceName, parentName);
    if (!parentName) return;
    let childNames = children.get(parentName);
    if (!childNames) {
      childNames = [];
      children.set(parentName, childNames);
    }
    if (!childNames.includes(interfaceName)) childNames.push(interfaceName);
  }

  function attachPrototypeToParent(interfaceName, ctor) {
    const parentName = parents.get(interfaceName);
    const parent = parentName ? ensureInterfaceConstructor(parentName) : null;
    if (parent && Object.getPrototypeOf(ctor.prototype) !== parent.prototype) {
      Object.setPrototypeOf(ctor.prototype, parent.prototype);
    }
  }

  function refreshDescendantPrototypeChains(interfaceName) {
    const childNames = children.get(interfaceName) || [];
    for (const childName of childNames) {
      const child = ensureInterfaceConstructor(childName);
      attachPrototypeToParent(childName, child);
      refreshDescendantPrototypeChains(childName);
    }
  }

  function defineInterface(interfaceName, parentName, attributes, operations) {
    const ctor = ensureInterfaceConstructor(interfaceName);
    rememberParent(interfaceName, parentName);
    constructors.set(interfaceName, ctor);
    attachPrototypeToParent(interfaceName, ctor);
    Object.defineProperty(ctor.prototype, Symbol.toStringTag, {
      value: interfaceName,
      configurable: true,
    });

    for (const name of attributes) {
      if (!Object.prototype.hasOwnProperty.call(ctor.prototype, name)) {
        Object.defineProperty(ctor.prototype, name, {
          get: unsupportedMember(interfaceName, name),
          enumerable: true,
          configurable: true,
        });
      }
    }
    for (const name of operations) {
      if (!Object.prototype.hasOwnProperty.call(ctor.prototype, name)) {
        Object.defineProperty(ctor.prototype, name, {
          value: unsupportedMember(interfaceName, name),
          writable: true,
          enumerable: true,
          configurable: true,
        });
      }
    }
    defineGlobalInterface(interfaceName, ctor);
    return ctor;
  }
"#;

const WEBIDL_BINDING_FOOTER: &str = r#"

  Object.defineProperty(globalThis, '__vixenWebidl', {
    value: Object.freeze({
      interfaceConstructor(name) {
        const ctor = constructors.get(String(name));
        if (!ctor) throw new TypeError('unknown Vixen WebIDL interface: ' + name);
        return ctor;
      },
      adoptInterface(name, implementation) {
        const base = constructors.get(String(name));
        if (!base) throw new TypeError('unknown Vixen WebIDL interface: ' + name);
        if (Object.getPrototypeOf(implementation.prototype) !== base.prototype) {
          Object.setPrototypeOf(implementation.prototype, base.prototype);
        }
        Object.defineProperty(implementation.prototype, Symbol.toStringTag, {
          value: String(name),
          configurable: true,
        });
        constructors.set(String(name), implementation);
        Object.defineProperty(globalThis, String(name), {
          value: implementation,
          writable: true,
          configurable: true,
        });
        refreshDescendantPrototypeChains(String(name));
        return implementation;
      },
      interfaceNames() {
        return Array.from(constructors.keys());
      },
    }),
    configurable: true,
  });
})();
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn generated_webidl_bootstrap_is_deterministic_and_browser_shaped() {
        let source = generated_webidl_bootstrap();

        assert!(source.is_ascii());
        assert!(source.contains("defineInterface(\"Document\", \"Node\""));
        assert!(source.contains("defineInterface(\"Element\", \"Node\""));
        assert!(source.contains("defineInterface(\"CSSStyleRule\", \"CSSRule\""));
        assert!(source.contains("getBoundingClientRect"));
        assert!(source.contains("getComputedStyle") || source.contains("CSSStyleDeclaration"));
        assert!(source.contains("refreshDescendantPrototypeChains"));
        assert!(source.contains("adoptInterface"));
    }

    #[test]
    fn generated_webidl_bootstrap_covers_every_manifest_interface() {
        let source = generated_webidl_bootstrap();

        for interface in WEBIDL_INTERFACES {
            assert!(
                source.contains(&format!("defineInterface(\"{}\"", interface.name)),
                "generated bootstrap omitted {}",
                interface.name
            );
        }
    }

    #[test]
    fn webidl_manifest_names_are_unique_and_parents_are_known() {
        let names = WEBIDL_INTERFACES
            .iter()
            .map(|interface| interface.name)
            .collect::<HashSet<_>>();

        assert_eq!(
            names.len(),
            WEBIDL_INTERFACES.len(),
            "duplicate WebIDL interface name"
        );
        for interface in WEBIDL_INTERFACES {
            if let Some(parent) = interface.parent {
                assert!(
                    names.contains(parent),
                    "{} inherits from unknown WebIDL parent {parent}",
                    interface.name
                );
            }
        }
    }
}
