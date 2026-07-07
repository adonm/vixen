//! Encoding API host extension for the JS runtime.

#![forbid(unsafe_code)]

use std::borrow::Cow;
use std::sync::Arc;

use deno_core::{Extension, ExtensionFileSource, JsBuffer};

use crate::text_codec::{TextDecoder, TextEncoder};

deno_core::extension!(
    vixen_encoding,
    ops = [
        op_vixen_text_encode,
        op_vixen_text_encode_into,
        op_vixen_text_decode,
    ],
);

pub(super) fn extension() -> Extension {
    let mut extension = vixen_encoding::init();
    extension.js_files = Cow::Owned(vec![ExtensionFileSource::new_computed(
        "ext:vixen_encoding/bootstrap.js",
        Arc::<str>::from(ENCODING_API_BOOTSTRAP),
    )]);
    extension
}

#[deno_core::op2]
#[serde]
fn op_vixen_text_encode(#[string] input: String) -> Vec<u8> {
    TextEncoder.encode(&input)
}

#[deno_core::op2]
#[serde]
fn op_vixen_text_encode_into(
    #[string] input: String,
    max_bytes: u32,
) -> deno_core::serde_json::Value {
    let mut bytes = vec![0; (max_bytes as usize).min(input.len())];
    let result = TextEncoder.encode_into(&input, &mut bytes);
    bytes.truncate(result.written);

    deno_core::serde_json::json!({
        "bytes": bytes,
        "read": result.read_utf16,
        "written": result.written,
    })
}

#[deno_core::op2]
#[serde]
fn op_vixen_text_decode(
    #[buffer] input: JsBuffer,
    fatal: bool,
    ignore_bom: bool,
) -> deno_core::serde_json::Value {
    let decoder = TextDecoder::new("utf-8", fatal, ignore_bom)
        .expect("utf-8 must be supported by TextDecoder");
    match decoder.decode(input.as_ref()) {
        Ok(value) => deno_core::serde_json::json!({ "ok": true, "value": value }),
        Err(_) => deno_core::serde_json::json!({
            "ok": false,
            "message": "The encoded data was not valid UTF-8",
        }),
    }
}

const ENCODING_API_BOOTSTRAP: &str = r#"
(() => {
  const {
    op_vixen_text_encode,
    op_vixen_text_encode_into,
    op_vixen_text_decode,
  } = Deno.core.ops;

  function validateLabel(label) {
    const value = String(label).trim().toLowerCase();
    if (value === '' || value === 'utf-8' || value === 'utf8') return 'utf-8';
    throw new RangeError('Vixen TextDecoder supports UTF-8 only');
  }

  function inputBytes(input) {
    if (input == null) return new Uint8Array();
    if (input instanceof Uint8Array) return input;
    if (input instanceof ArrayBuffer) return new Uint8Array(input);
    if (ArrayBuffer.isView(input)) {
      return new Uint8Array(input.buffer, input.byteOffset, input.byteLength);
    }
    return new Uint8Array(input);
  }

  class TextEncoder {
    get encoding() { return 'utf-8'; }
    encode(input = '') {
      return new Uint8Array(op_vixen_text_encode(String(input)));
    }
    encodeInto(input = '', destination) {
      if (!(destination instanceof Uint8Array)) {
        throw new TypeError('TextEncoder.encodeInto destination must be a Uint8Array');
      }
      const result = op_vixen_text_encode_into(String(input), destination.length);
      destination.set(result.bytes);
      return { read: result.read, written: result.written };
    }
  }

  class TextDecoder {
    constructor(label = 'utf-8', options = {}) {
      const opts = options == null ? {} : Object(options);
      this.__vixenLabel = validateLabel(label);
      this.__vixenFatal = !!opts.fatal;
      this.__vixenIgnoreBOM = !!opts.ignoreBOM;
    }
    get encoding() { return 'utf-8'; }
    get fatal() { return this.__vixenFatal; }
    get ignoreBOM() { return this.__vixenIgnoreBOM; }
    decode(input = new Uint8Array()) {
      const result = op_vixen_text_decode(
        inputBytes(input),
        this.__vixenFatal,
        this.__vixenIgnoreBOM,
      );
      if (!result.ok) throw new TypeError(result.message);
      return result.value;
    }
  }

  Object.defineProperty(globalThis, 'TextEncoder', {
    value: TextEncoder,
    writable: true,
    configurable: true,
  });
  Object.defineProperty(globalThis, 'TextDecoder', {
    value: TextDecoder,
    writable: true,
    configurable: true,
  });
})();
"#;
