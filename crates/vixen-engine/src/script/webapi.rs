//! JavaScript-only browser value-object compatibility layer.
//!
//! These bindings deliberately stay in `deno_core`/V8: they fill WebIDL-shaped
//! constructor/prototype behavior for pure value APIs before a backend is
//! involved. Page-backed DOM objects live in `script::dom`.

#![forbid(unsafe_code)]

use std::borrow::Cow;
use std::sync::Arc;

use deno_core::{Extension, ExtensionFileSource};

deno_core::extension!(vixen_webapi);

pub(super) fn extension() -> Extension {
    let mut extension = vixen_webapi::init();
    extension.js_files = Cow::Owned(vec![ExtensionFileSource::new_computed(
        "ext:vixen_webapi/bootstrap.js",
        Arc::<str>::from(WEB_API_BOOTSTRAP),
    )]);
    extension
}

const WEB_API_BOOTSTRAP: &str = r#"
(() => {
  const webidl = globalThis.__vixenWebidl;
  const textEncoder = typeof TextEncoder === 'function' ? new TextEncoder() : null;
  const startEpoch = Date.now();

  function defineGlobal(name, value) {
    Object.defineProperty(globalThis, name, {
      value,
      writable: true,
      configurable: true,
    });
  }

  function defineReadonly(target, name, value, enumerable = true) {
    Object.defineProperty(target, name, {
      value,
      enumerable,
      configurable: true,
    });
  }

  function defineData(target, name, value, enumerable = true) {
    Object.defineProperty(target, name, {
      value,
      writable: true,
      enumerable,
      configurable: true,
    });
  }

  function copyPrototypeMembers(source, target) {
    for (const name of Reflect.ownKeys(source)) {
      if (name === 'constructor') continue;
      if (Object.prototype.hasOwnProperty.call(target, name)) continue;
      Object.defineProperty(target, name, Object.getOwnPropertyDescriptor(source, name));
    }
  }

  function finiteNumber(value, fallback = 0) {
    const number = Number(value);
    return Number.isFinite(number) ? number : fallback;
  }

  function byteLength(value) {
    const string = String(value);
    return textEncoder ? textEncoder.encode(string).length : string.length;
  }

  // -----------------------------------------------------------------------
  // Event / EventTarget
  // -----------------------------------------------------------------------

  const listeners = new WeakMap();
  const eventState = new WeakMap();

  function listenerList(target, type, create) {
    let byType = listeners.get(target);
    if (!byType && create) {
      byType = new Map();
      listeners.set(target, byType);
    }
    if (!byType) return undefined;
    let list = byType.get(type);
    if (!list && create) {
      list = [];
      byType.set(type, list);
    }
    return list;
  }

  class VixenEventTarget {
    addEventListener(type, callback) {
      if (callback === null || callback === undefined) return;
      const eventType = String(type);
      const list = listenerList(this, eventType, true);
      if (!list.includes(callback)) list.push(callback);
    }
    removeEventListener(type, callback) {
      const list = listenerList(this, String(type), false);
      if (!list) return;
      const index = list.indexOf(callback);
      if (index >= 0) list.splice(index, 1);
    }
    dispatchEvent(event) {
      const state = eventState.get(event);
      if (!state) throw new TypeError('dispatchEvent expects an Event');
      state.target = this;
      state.currentTarget = this;
      state.eventPhase = 2;
      state.path = [this];
      const list = listenerList(this, state.type, false) || [];
      for (const callback of list.slice()) {
        if (state.immediateStopped) break;
        if (typeof callback === 'function') {
          callback.call(this, event);
        } else if (callback && typeof callback.handleEvent === 'function') {
          callback.handleEvent(event);
        }
      }
      state.currentTarget = null;
      state.eventPhase = 0;
      return !(state.cancelable && state.defaultPrevented);
    }
  }

  class VixenEvent {
    constructor(type, init = {}) {
      eventState.set(this, {
        type: String(type),
        bubbles: Boolean(init && init.bubbles),
        cancelable: Boolean(init && init.cancelable),
        composed: Boolean(init && init.composed),
        defaultPrevented: false,
        stopped: false,
        immediateStopped: false,
        target: null,
        currentTarget: null,
        eventPhase: 0,
        timeStamp: performance.now(),
        path: [],
      });
    }
    get type() { return eventState.get(this).type; }
    get target() { return eventState.get(this).target; }
    get currentTarget() { return eventState.get(this).currentTarget; }
    get eventPhase() { return eventState.get(this).eventPhase; }
    get bubbles() { return eventState.get(this).bubbles; }
    get cancelable() { return eventState.get(this).cancelable; }
    get defaultPrevented() { return eventState.get(this).defaultPrevented; }
    get composed() { return eventState.get(this).composed; }
    get timeStamp() { return eventState.get(this).timeStamp; }
    get isTrusted() { return false; }
    stopPropagation() { eventState.get(this).stopped = true; }
    stopImmediatePropagation() {
      const state = eventState.get(this);
      state.stopped = true;
      state.immediateStopped = true;
    }
    preventDefault() {
      const state = eventState.get(this);
      if (state.cancelable) state.defaultPrevented = true;
    }
    composedPath() { return eventState.get(this).path.slice(); }
  }

  class VixenCustomEvent extends VixenEvent {
    constructor(type, init = {}) {
      super(type, init);
      defineReadonly(this, '__vixenDetail', init && Object.prototype.hasOwnProperty.call(init, 'detail') ? init.detail : null, false);
    }
    get detail() { return this.__vixenDetail; }
    initCustomEvent() {}
  }

  webidl.adoptInterface('EventTarget', VixenEventTarget);
  webidl.adoptInterface('Event', VixenEvent);
  webidl.adoptInterface('CustomEvent', VixenCustomEvent);

  // -----------------------------------------------------------------------
  // Geometry Interfaces
  // -----------------------------------------------------------------------

  function pointInit(init) {
    init = init || {};
    return {
      x: finiteNumber(init.x, 0),
      y: finiteNumber(init.y, 0),
      z: finiteNumber(init.z, 0),
      w: init.w === undefined ? 1 : finiteNumber(init.w, 1),
    };
  }

  class VixenDOMPointReadOnly {
    constructor(x = 0, y = 0, z = 0, w = 1) {
      defineData(this, 'x', finiteNumber(x, 0));
      defineData(this, 'y', finiteNumber(y, 0));
      defineData(this, 'z', finiteNumber(z, 0));
      defineData(this, 'w', finiteNumber(w, 1));
    }
    matrixTransform(matrix = new VixenDOMMatrix()) {
      return new VixenDOMMatrix(matrix).transformPoint(this);
    }
    toJSON() { return { x: this.x, y: this.y, z: this.z, w: this.w }; }
  }

  class VixenDOMPoint extends VixenDOMPointReadOnly {
    static fromPoint(init = {}) {
      const point = pointInit(init);
      return new VixenDOMPoint(point.x, point.y, point.z, point.w);
    }
  }

  function rectInit(init) {
    init = init || {};
    return {
      x: finiteNumber(init.x, 0),
      y: finiteNumber(init.y, 0),
      width: finiteNumber(init.width, 0),
      height: finiteNumber(init.height, 0),
    };
  }

  class VixenDOMRectReadOnly {
    constructor(x = 0, y = 0, width = 0, height = 0) {
      defineData(this, 'x', finiteNumber(x, 0));
      defineData(this, 'y', finiteNumber(y, 0));
      defineData(this, 'width', finiteNumber(width, 0));
      defineData(this, 'height', finiteNumber(height, 0));
    }
    get left() { return Math.min(this.x, this.x + this.width); }
    get top() { return Math.min(this.y, this.y + this.height); }
    get right() { return Math.max(this.x, this.x + this.width); }
    get bottom() { return Math.max(this.y, this.y + this.height); }
    toJSON() {
      return {
        x: this.x,
        y: this.y,
        width: this.width,
        height: this.height,
        top: this.top,
        right: this.right,
        bottom: this.bottom,
        left: this.left,
      };
    }
    static fromRect(init = {}) {
      const rect = rectInit(init);
      return new VixenDOMRectReadOnly(rect.x, rect.y, rect.width, rect.height);
    }
  }

  class VixenDOMRect extends VixenDOMRectReadOnly {
    static fromRect(init = {}) {
      const rect = rectInit(init);
      return new VixenDOMRect(rect.x, rect.y, rect.width, rect.height);
    }
  }

  class VixenDOMQuad {
    constructor(p1 = {}, p2 = {}, p3 = {}, p4 = {}) {
      const a = pointInit(p1), b = pointInit(p2), c = pointInit(p3), d = pointInit(p4);
      defineData(this, 'p1', new VixenDOMPoint(a.x, a.y, a.z, a.w));
      defineData(this, 'p2', new VixenDOMPoint(b.x, b.y, b.z, b.w));
      defineData(this, 'p3', new VixenDOMPoint(c.x, c.y, c.z, c.w));
      defineData(this, 'p4', new VixenDOMPoint(d.x, d.y, d.z, d.w));
    }
    static fromRect(init = {}) {
      const r = VixenDOMRect.fromRect(init);
      return new VixenDOMQuad(
        { x: r.left, y: r.top },
        { x: r.right, y: r.top },
        { x: r.right, y: r.bottom },
        { x: r.left, y: r.bottom },
      );
    }
    static fromQuad(init = {}) {
      return new VixenDOMQuad(init.p1, init.p2, init.p3, init.p4);
    }
    getBounds() {
      const xs = [this.p1.x, this.p2.x, this.p3.x, this.p4.x];
      const ys = [this.p1.y, this.p2.y, this.p3.y, this.p4.y];
      const left = Math.min(...xs), right = Math.max(...xs);
      const top = Math.min(...ys), bottom = Math.max(...ys);
      return new VixenDOMRect(left, top, right - left, bottom - top);
    }
    toJSON() { return { p1: this.p1.toJSON(), p2: this.p2.toJSON(), p3: this.p3.toJSON(), p4: this.p4.toJSON() }; }
  }

  function identityMatrix() {
    return [1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1];
  }

  function matrixFromInit(init) {
    if (init === undefined || init === null) return identityMatrix();
    if (init instanceof VixenDOMMatrixReadOnly) return init.__vixenMatrix.slice();
    if (ArrayBuffer.isView(init) || Array.isArray(init)) {
      const values = Array.from(init, (value) => finiteNumber(value, 0));
      if (values.length === 6) {
        return [values[0], values[1], 0, 0, values[2], values[3], 0, 0, 0, 0, 1, 0, values[4], values[5], 0, 1];
      }
      if (values.length === 16) return values.slice();
      throw new TypeError('DOMMatrix sequence must have 6 or 16 numbers');
    }
    if (typeof init === 'object') {
      const m = identityMatrix();
      const names = ['m11','m12','m13','m14','m21','m22','m23','m24','m31','m32','m33','m34','m41','m42','m43','m44'];
      for (let i = 0; i < names.length; i++) if (init[names[i]] !== undefined) m[i] = finiteNumber(init[names[i]], m[i]);
      for (const [alias, index] of [['a',0],['b',1],['c',4],['d',5],['e',12],['f',13]]) {
        if (init[alias] !== undefined) m[index] = finiteNumber(init[alias], m[index]);
      }
      return m;
    }
    throw new TypeError('unsupported DOMMatrix init');
  }

  function multiplyMatrix(a, b) {
    const out = new Array(16).fill(0);
    for (let col = 0; col < 4; col++) {
      for (let row = 0; row < 4; row++) {
        let sum = 0;
        for (let k = 0; k < 4; k++) sum += a[k * 4 + row] * b[col * 4 + k];
        out[col * 4 + row] = sum;
      }
    }
    return out;
  }

  function translatedMatrix(tx, ty, tz) {
    const m = identityMatrix();
    m[12] = tx; m[13] = ty; m[14] = tz;
    return m;
  }

  function scaledMatrix(sx, sy, sz) {
    const m = identityMatrix();
    m[0] = sx; m[5] = sy; m[10] = sz;
    return m;
  }

  function rotationMatrix(angle) {
    const t = finiteNumber(angle, 0) * Math.PI / 180;
    const c = Math.cos(t), s = Math.sin(t);
    return [c, s, 0, 0, -s, c, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1];
  }

  function skewXMatrix(angle) {
    const t = Math.tan(finiteNumber(angle, 0) * Math.PI / 180);
    return [1, 0, 0, 0, t, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1];
  }

  function skewYMatrix(angle) {
    const t = Math.tan(finiteNumber(angle, 0) * Math.PI / 180);
    return [1, t, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1];
  }

  class VixenDOMMatrixReadOnly {
    constructor(init) {
      defineReadonly(this, '__vixenMatrix', matrixFromInit(init), false);
    }
    get m11() { return this.__vixenMatrix[0]; } get a() { return this.m11; }
    get m12() { return this.__vixenMatrix[1]; } get b() { return this.m12; }
    get m13() { return this.__vixenMatrix[2]; }
    get m14() { return this.__vixenMatrix[3]; }
    get m21() { return this.__vixenMatrix[4]; } get c() { return this.m21; }
    get m22() { return this.__vixenMatrix[5]; } get d() { return this.m22; }
    get m23() { return this.__vixenMatrix[6]; }
    get m24() { return this.__vixenMatrix[7]; }
    get m31() { return this.__vixenMatrix[8]; }
    get m32() { return this.__vixenMatrix[9]; }
    get m33() { return this.__vixenMatrix[10]; }
    get m34() { return this.__vixenMatrix[11]; }
    get m41() { return this.__vixenMatrix[12]; } get e() { return this.m41; }
    get m42() { return this.__vixenMatrix[13]; } get f() { return this.m42; }
    get m43() { return this.__vixenMatrix[14]; }
    get m44() { return this.__vixenMatrix[15]; }
    get is2D() {
      const m = this.__vixenMatrix;
      return m[2] === 0 && m[3] === 0 && m[6] === 0 && m[7] === 0 &&
        m[8] === 0 && m[9] === 0 && m[11] === 0 && m[14] === 0 && m[10] === 1 && m[15] === 1;
    }
    get isIdentity() { return this.__vixenMatrix.every((v, i) => v === identityMatrix()[i]); }
    _new(matrix) { return new VixenDOMMatrix(matrix); }
    multiply(other = undefined) { return this._new(multiplyMatrix(this.__vixenMatrix, matrixFromInit(other))); }
    translate(tx = 0, ty = 0, tz = 0) { return this._new(multiplyMatrix(this.__vixenMatrix, translatedMatrix(finiteNumber(tx), finiteNumber(ty), finiteNumber(tz)))); }
    scale(sx = 1, sy = sx, sz = 1) { return this._new(multiplyMatrix(this.__vixenMatrix, scaledMatrix(finiteNumber(sx, 1), finiteNumber(sy, finiteNumber(sx, 1)), finiteNumber(sz, 1)))); }
    rotate(angle = 0) { return this._new(multiplyMatrix(this.__vixenMatrix, rotationMatrix(angle))); }
    skewX(angle = 0) { return this._new(multiplyMatrix(this.__vixenMatrix, skewXMatrix(angle))); }
    skewY(angle = 0) { return this._new(multiplyMatrix(this.__vixenMatrix, skewYMatrix(angle))); }
    flipX() { return this.scale(-1, 1, 1); }
    flipY() { return this.scale(1, -1, 1); }
    inverse() { throw new TypeError('DOMMatrix.inverse is not implemented by Vixen yet'); }
    transformPoint(point = {}) {
      const p = pointInit(point);
      const m = this.__vixenMatrix;
      const x = m[0] * p.x + m[4] * p.y + m[8] * p.z + m[12] * p.w;
      const y = m[1] * p.x + m[5] * p.y + m[9] * p.z + m[13] * p.w;
      const z = m[2] * p.x + m[6] * p.y + m[10] * p.z + m[14] * p.w;
      const w = m[3] * p.x + m[7] * p.y + m[11] * p.z + m[15] * p.w;
      return w === 0 || !Number.isFinite(w) ? new VixenDOMPoint(x, y, z, w) : new VixenDOMPoint(x / w, y / w, z / w, 1);
    }
    toFloat32Array() { return new Float32Array(this.__vixenMatrix); }
    toFloat64Array() { return new Float64Array(this.__vixenMatrix); }
    toJSON() {
      const out = {};
      for (const name of ['a','b','c','d','e','f','m11','m12','m13','m14','m21','m22','m23','m24','m31','m32','m33','m34','m41','m42','m43','m44','is2D','isIdentity']) out[name] = this[name];
      return out;
    }
  }

  class VixenDOMMatrix extends VixenDOMMatrixReadOnly {
    static fromMatrix(init = {}) { return new VixenDOMMatrix(init); }
    static fromFloat32Array(array) { return new VixenDOMMatrix(array); }
    static fromFloat64Array(array) { return new VixenDOMMatrix(array); }
    _replace(matrix) { this.__vixenMatrix.splice(0, 16, ...matrix); return this; }
    multiplySelf(other) { return this._replace(multiplyMatrix(this.__vixenMatrix, matrixFromInit(other))); }
    preMultiplySelf(other) { return this._replace(multiplyMatrix(matrixFromInit(other), this.__vixenMatrix)); }
    translateSelf(tx = 0, ty = 0, tz = 0) { return this._replace(this.translate(tx, ty, tz).__vixenMatrix); }
    scaleSelf(sx = 1, sy = sx, sz = 1) { return this._replace(this.scale(sx, sy, sz).__vixenMatrix); }
    rotateSelf(angle = 0) { return this._replace(this.rotate(angle).__vixenMatrix); }
    skewXSelf(angle = 0) { return this._replace(this.skewX(angle).__vixenMatrix); }
    skewYSelf(angle = 0) { return this._replace(this.skewY(angle).__vixenMatrix); }
    invertSelf() { return this._replace(this.inverse().__vixenMatrix); }
    setMatrixValue() { throw new TypeError('DOMMatrix.setMatrixValue is not implemented by Vixen yet'); }
  }

  copyPrototypeMembers(VixenDOMMatrixReadOnly.prototype, VixenDOMMatrix.prototype);

  webidl.adoptInterface('DOMPointReadOnly', VixenDOMPointReadOnly);
  webidl.adoptInterface('DOMPoint', VixenDOMPoint);
  webidl.adoptInterface('DOMRectReadOnly', VixenDOMRectReadOnly);
  webidl.adoptInterface('DOMRect', VixenDOMRect);
  webidl.adoptInterface('DOMQuad', VixenDOMQuad);
  webidl.adoptInterface('DOMMatrixReadOnly', VixenDOMMatrixReadOnly);
  webidl.adoptInterface('DOMMatrix', VixenDOMMatrix);

  // -----------------------------------------------------------------------
  // URL / URLSearchParams / URLPattern
  // -----------------------------------------------------------------------

  function decodeParam(value) {
    return decodeURIComponent(String(value).replace(/\+/g, ' '));
  }

  function encodeParam(value) {
    return encodeURIComponent(String(value)).replace(/%20/g, '+');
  }

  class VixenURLSearchParams {
    constructor(init = '') {
      defineReadonly(this, '__vixenPairs', [], false);
      if (typeof init === 'string') {
        let input = init.startsWith('?') ? init.slice(1) : init;
        if (input !== '') {
          for (const part of input.split('&')) {
            const [name, value = ''] = part.split('=');
            this.append(decodeParam(name), decodeParam(value));
          }
        }
      } else if (init && typeof init[Symbol.iterator] === 'function') {
        for (const pair of init) this.append(pair[0], pair[1]);
      } else if (init && typeof init === 'object') {
        for (const [name, value] of Object.entries(init)) this.append(name, value);
      }
    }
    get size() { return this.__vixenPairs.length; }
    append(name, value) { this.__vixenPairs.push([String(name), String(value)]); }
    delete(name) { const n = String(name); for (let i = this.__vixenPairs.length - 1; i >= 0; i--) if (this.__vixenPairs[i][0] === n) this.__vixenPairs.splice(i, 1); }
    get(name) { const n = String(name); const pair = this.__vixenPairs.find(([key]) => key === n); return pair ? pair[1] : null; }
    getAll(name) { const n = String(name); return this.__vixenPairs.filter(([key]) => key === n).map(([, value]) => value); }
    has(name, value = undefined) { const n = String(name); return this.__vixenPairs.some(([key, val]) => key === n && (value === undefined || val === String(value))); }
    set(name, value) {
      const n = String(name), v = String(value);
      let found = false;
      for (let i = this.__vixenPairs.length - 1; i >= 0; i--) {
        if (this.__vixenPairs[i][0] === n) {
          if (!found) { this.__vixenPairs[i][1] = v; found = true; }
          else this.__vixenPairs.splice(i, 1);
        }
      }
      if (!found) this.append(n, v);
    }
    sort() { this.__vixenPairs.sort((a, b) => a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : 0); }
    entries() { return this.__vixenPairs.map((pair) => pair.slice())[Symbol.iterator](); }
    keys() { return this.__vixenPairs.map(([name]) => name)[Symbol.iterator](); }
    values() { return this.__vixenPairs.map(([, value]) => value)[Symbol.iterator](); }
    forEach(callback, thisArg = undefined) { for (const [name, value] of this.__vixenPairs) callback.call(thisArg, value, name, this); }
    toString() { return this.__vixenPairs.map(([name, value]) => encodeParam(name) + '=' + encodeParam(value)).join('&'); }
    [Symbol.iterator]() { return this.entries(); }
  }

  function parseUrl(input, base = undefined) {
    input = String(input);
    if (base !== undefined && !/^[A-Za-z][A-Za-z0-9+.-]*:/.test(input)) {
      const b = parseUrl(base);
      if (input.startsWith('/')) input = b.protocol + '//' + b.host + input;
      else {
        const dir = b.pathname.replace(/[^/]*$/, '');
        input = b.protocol + '//' + b.host + dir + input;
      }
    }
    const data = input.match(/^([A-Za-z][A-Za-z0-9+.-]*):(.*)$/);
    if (!data) throw new TypeError('Invalid URL');
    const scheme = data[1].toLowerCase();
    if (scheme === 'data') {
      return { protocol: 'data:', username: '', password: '', host: '', hostname: '', port: '', pathname: data[2], search: '', hash: '', origin: 'null', href: 'data:' + data[2] };
    }
    const match = input.match(/^([A-Za-z][A-Za-z0-9+.-]*):\/\/([^/?#]*)([^?#]*)(\?[^#]*)?(#.*)?$/);
    if (!match) throw new TypeError('Invalid URL');
    let authority = match[2];
    let username = '', password = '';
    if (authority.includes('@')) {
      const parts = authority.split('@');
      const auth = parts.shift();
      authority = parts.join('@');
      const [u, p = ''] = auth.split(':');
      username = u; password = p;
    }
    let hostname = authority, port = '';
    if (authority.startsWith('[')) {
      const end = authority.indexOf(']');
      hostname = authority.slice(0, end + 1);
      if (authority.slice(end + 1).startsWith(':')) port = authority.slice(end + 2);
    } else if (authority.includes(':')) {
      const pieces = authority.split(':');
      port = pieces.pop();
      hostname = pieces.join(':');
    }
    const pathname = match[3] || '/';
    const search = match[4] || '';
    const hash = match[5] || '';
    const host = hostname + (port ? ':' + port : '');
    const protocol = scheme + ':';
    return { protocol, username, password, host, hostname, port, pathname, search, hash, origin: protocol + '//' + host, href: protocol + '//' + host + pathname + search + hash };
  }

  class VixenURL {
    constructor(input, base = undefined) {
      const parsed = parseUrl(input, base);
      for (const key of Object.keys(parsed)) defineReadonly(this, key === 'href' ? '__vixenHref' : '__vixen' + key[0].toUpperCase() + key.slice(1), parsed[key], false);
      defineReadonly(this, 'searchParams', new VixenURLSearchParams(parsed.search), true);
    }
    get href() { return this.__vixenHref; }
    get origin() { return this.__vixenOrigin; }
    get protocol() { return this.__vixenProtocol; }
    get username() { return this.__vixenUsername; }
    get password() { return this.__vixenPassword; }
    get host() { return this.__vixenHost; }
    get hostname() { return this.__vixenHostname; }
    get port() { return this.__vixenPort; }
    get pathname() { return this.__vixenPathname; }
    get search() { return this.__vixenSearch; }
    get hash() { return this.__vixenHash; }
    toString() { return this.href; }
    toJSON() { return this.href; }
    static canParse(input, base = undefined) { try { parseUrl(input, base); return true; } catch (_) { return false; } }
    static parse(input, base = undefined) { return new VixenURL(input, base); }
    static createObjectURL() { return 'blob:vixen'; }
    static revokeObjectURL() {}
  }

  class VixenURLPattern {
    constructor(init = {}) { defineReadonly(this, 'pathname', String(init.pathname || '*'), true); }
    _match(input) {
      const path = String((input && input.pathname) || '');
      const pattern = this.pathname;
      if (pattern.endsWith('/*')) {
        const prefix = pattern.slice(0, -1);
        if (!path.startsWith(prefix)) return null;
        return { '*': path.slice(prefix.length) };
      }
      const p = pattern.split('/').filter(Boolean);
      const s = path.split('/').filter(Boolean);
      if (p.length !== s.length) return null;
      const groups = {};
      for (let i = 0; i < p.length; i++) {
        if (p[i].startsWith(':')) groups[p[i].slice(1)] = s[i];
        else if (p[i] !== s[i]) return null;
      }
      return groups;
    }
    test(input) { return this._match(input) !== null; }
    exec(input) { const groups = this._match(input); return groups === null ? null : { pathname: { groups } }; }
  }

  webidl.adoptInterface('URL', VixenURL);
  webidl.adoptInterface('URLSearchParams', VixenURLSearchParams);
  webidl.adoptInterface('URLPattern', VixenURLPattern);

  // -----------------------------------------------------------------------
  // Fetch body value APIs: Headers / Blob / File / Request / Response
  // -----------------------------------------------------------------------

  function normalizeHeaderName(name) {
    const value = String(name);
    if (!/^[!#$%&'*+.^_`|~0-9A-Za-z-]+$/.test(value)) throw new TypeError('invalid header name');
    return value.toLowerCase();
  }

  function normalizeHeaderValue(value) {
    const text = String(value);
    if (/[\0\r\n]/.test(text)) throw new TypeError('invalid header value');
    return text.replace(/^[\t ]+|[\t ]+$/g, '');
  }

  function forbiddenRequestHeader(name) {
    name = String(name).toLowerCase();
    return ['accept-charset','accept-encoding','access-control-request-headers','access-control-request-method','connection','content-length','cookie','cookie2','date','dnt','expect','host','keep-alive','origin','referer','set-cookie','te','trailer','transfer-encoding','upgrade','via'].includes(name) || name.startsWith('proxy-') || name.startsWith('sec-');
  }

  function forbiddenResponseHeader(name) {
    name = String(name).toLowerCase();
    return name === 'set-cookie' || name === 'set-cookie2';
  }

  class VixenHeaders {
    constructor(init = undefined) {
      defineReadonly(this, '__vixenEntries', [], false);
      if (init instanceof VixenHeaders) {
        for (const [name, value] of init.__vixenEntries) this.append(name, value);
      } else if (init && typeof init[Symbol.iterator] === 'function') {
        for (const pair of init) this.append(pair[0], pair[1]);
      } else if (init && typeof init === 'object') {
        for (const [name, value] of Object.entries(init)) this.append(name, value);
      }
    }
    append(name, value) { this.__vixenEntries.push([normalizeHeaderName(name), normalizeHeaderValue(value)]); }
    delete(name) { const n = normalizeHeaderName(name); for (let i = this.__vixenEntries.length - 1; i >= 0; i--) if (this.__vixenEntries[i][0] === n) this.__vixenEntries.splice(i, 1); }
    get(name) { const n = normalizeHeaderName(name); const values = this.__vixenEntries.filter(([key]) => key === n).map(([, value]) => value); return values.length ? values.join(', ') : null; }
    getAll(name) { const n = normalizeHeaderName(name); return this.__vixenEntries.filter(([key]) => key === n).map(([, value]) => value); }
    getSetCookie() { return []; }
    has(name) { const n = normalizeHeaderName(name); return this.__vixenEntries.some(([key]) => key === n); }
    set(name, value) { const n = normalizeHeaderName(name); this.delete(n); this.__vixenEntries.push([n, normalizeHeaderValue(value)]); }
    get size() { return Array.from(new Set(this.__vixenEntries.map(([name]) => name))).length; }
    _combined() {
      const out = [];
      for (const [name] of this.__vixenEntries) if (!out.some(([existing]) => existing === name)) out.push([name, this.get(name)]);
      return out;
    }
    entries() { return this._combined()[Symbol.iterator](); }
    keys() { return this._combined().map(([name]) => name)[Symbol.iterator](); }
    values() { return this._combined().map(([, value]) => value)[Symbol.iterator](); }
    forEach(callback, thisArg = undefined) { for (const [name, value] of this._combined()) callback.call(thisArg, value, name, this); }
    [Symbol.iterator]() { return this.entries(); }
  }

  function filteredHeaders(init, forbidden) {
    const headers = new VixenHeaders(init);
    if (!forbidden) return headers;
    const filtered = new VixenHeaders();
    for (const [name, value] of headers.__vixenEntries) if (!forbidden(name)) filtered.append(name, value);
    return filtered;
  }

  function normalizeMime(type) {
    const value = String(type || '');
    return /^[\x20-\x7e]*$/.test(value) ? value.toLowerCase() : '';
  }

  class VixenBlob {
    constructor(parts = [], options = {}) {
      defineReadonly(this, '__vixenParts', Array.from(parts, (part) => part instanceof VixenBlob ? part.__vixenText : String(part)), false);
      defineReadonly(this, '__vixenText', this.__vixenParts.join(''), false);
      defineReadonly(this, 'size', byteLength(this.__vixenText), true);
      defineReadonly(this, 'type', normalizeMime(options && options.type), true);
    }
    slice(start = 0, end = this.size, type = '') { return new VixenBlob([this.__vixenText.slice(start, end)], { type }); }
    text() { return Promise.resolve(this.__vixenText); }
    arrayBuffer() { return Promise.resolve(textEncoder.encode(this.__vixenText).buffer); }
    bytes() { return Promise.resolve(textEncoder.encode(this.__vixenText)); }
    stream() { return null; }
  }

  class VixenFile extends VixenBlob {
    constructor(parts, name, options = {}) {
      super(parts, options);
      defineReadonly(this, 'name', String(name), true);
      defineReadonly(this, 'lastModified', options && options.lastModified !== undefined ? finiteNumber(options.lastModified, 0) : Date.now(), true);
      defineReadonly(this, 'webkitRelativePath', '', true);
    }
  }

  function bodyInfo(body) {
    if (body === null || body === undefined) return { isNull: true, contentType: '' };
    if (body instanceof VixenBlob) return { isNull: false, contentType: body.type };
    if (body instanceof VixenURLSearchParams) return { isNull: false, contentType: 'application/x-www-form-urlencoded;charset=UTF-8' };
    return { isNull: false, contentType: 'text/plain;charset=UTF-8' };
  }

  class VixenRequest {
    constructor(input, init = {}) {
      const source = input instanceof VixenRequest ? input : null;
      const url = source ? source.url : new VixenURL(String(input)).href;
      const method = String((init && init.method) || (source && source.method) || 'GET').toUpperCase();
      const body = bodyInfo(init && Object.prototype.hasOwnProperty.call(init, 'body') ? init.body : null);
      if (!body.isNull && (method === 'GET' || method === 'HEAD')) throw new TypeError('Request GET/HEAD cannot have a body');
      const headers = filteredHeaders((init && init.headers) || (source && source.headers) || undefined, forbiddenRequestHeader);
      if (!body.isNull && body.contentType && !headers.has('content-type')) headers.set('Content-Type', body.contentType);
      defineReadonly(this, 'url', url, true);
      defineReadonly(this, 'method', method, true);
      defineReadonly(this, 'headers', headers, true);
      defineReadonly(this, 'destination', '', true);
      defineReadonly(this, 'referrer', (init && init.referrer) || 'about:client', true);
      defineReadonly(this, 'referrerPolicy', (init && init.referrerPolicy) || '', true);
      defineReadonly(this, 'mode', (init && init.mode) || 'cors', true);
      defineReadonly(this, 'credentials', (init && init.credentials) || 'same-origin', true);
      defineReadonly(this, 'cache', (init && init.cache) || 'default', true);
      defineReadonly(this, 'redirect', (init && init.redirect) || 'follow', true);
      defineReadonly(this, 'integrity', (init && init.integrity) || '', true);
      defineReadonly(this, 'keepalive', Boolean(init && init.keepalive), true);
      defineReadonly(this, 'signal', (init && init.signal) || new VixenAbortController().signal, true);
      defineReadonly(this, 'body', body.isNull ? null : {}, true);
      defineReadonly(this, 'bodyUsed', false, true);
    }
    clone() { return new VixenRequest(this); }
    text() { return Promise.resolve(''); }
    json() { return Promise.resolve(null); }
    blob() { return Promise.resolve(new VixenBlob([])); }
    arrayBuffer() { return Promise.resolve(new ArrayBuffer(0)); }
    bytes() { return Promise.resolve(new Uint8Array()); }
    formData() { return Promise.resolve(new FormData()); }
  }

  class VixenResponse {
    constructor(body = null, init = {}) {
      const info = bodyInfo(body);
      const status = init && init.status !== undefined ? finiteNumber(init.status, 200) : 200;
      const headers = filteredHeaders(init && init.headers, forbiddenResponseHeader);
      if (!info.isNull && info.contentType && !headers.has('content-type')) headers.set('Content-Type', info.contentType);
      defineReadonly(this, 'type', 'default', true);
      defineReadonly(this, 'url', '', true);
      defineReadonly(this, 'redirected', false, true);
      defineReadonly(this, 'status', status, true);
      defineReadonly(this, 'ok', status >= 200 && status <= 299, true);
      defineReadonly(this, 'statusText', (init && init.statusText) || '', true);
      defineReadonly(this, 'headers', headers, true);
      defineReadonly(this, 'body', info.isNull ? null : {}, true);
      defineReadonly(this, 'bodyUsed', false, true);
    }
    clone() { return this; }
    text() { return Promise.resolve(''); }
    json() { return Promise.resolve(null); }
    blob() { return Promise.resolve(new VixenBlob([])); }
    arrayBuffer() { return Promise.resolve(new ArrayBuffer(0)); }
    bytes() { return Promise.resolve(new Uint8Array()); }
    formData() { return Promise.resolve(new FormData()); }
    static error() {
      const response = new VixenResponse(null, { status: 200 });
      Object.defineProperty(response, 'type', { value: 'error', configurable: true });
      Object.defineProperty(response, 'status', { value: 0, configurable: true });
      Object.defineProperty(response, 'ok', { value: false, configurable: true });
      return response;
    }
    static json(data, init = {}) {
      const response = new VixenResponse(JSON.stringify(data), init);
      if (!response.headers.has('content-type')) response.headers.set('Content-Type', 'application/json');
      return response;
    }
    static redirect(url, status = 302) { return new VixenResponse(null, { status, headers: [['Location', new VixenURL(url).href]] }); }
  }

  webidl.adoptInterface('Headers', VixenHeaders);
  webidl.adoptInterface('Blob', VixenBlob);
  webidl.adoptInterface('File', VixenFile);
  webidl.adoptInterface('Request', VixenRequest);
  webidl.adoptInterface('Response', VixenResponse);

  // -----------------------------------------------------------------------
  // Abort, MutationObserver, structuredClone, DOMParser, platform globals
  // -----------------------------------------------------------------------

  class VixenAbortSignal extends VixenEventTarget {
    constructor(aborted = false, reason = undefined) { super(); defineReadonly(this, '__vixenAbortState', { aborted, reason }, false); }
    get aborted() { return this.__vixenAbortState.aborted; }
    get reason() { return this.__vixenAbortState.reason; }
    throwIfAborted() { if (this.aborted) throw this.reason; }
    static abort(reason = undefined) { return new VixenAbortSignal(true, reason); }
    static timeout(_ms) { return new VixenAbortSignal(true, new Error('TimeoutError')); }
    static any(signals) { return new VixenAbortSignal(Array.from(signals).some((signal) => signal.aborted), undefined); }
  }

  class VixenAbortController {
    constructor() { defineReadonly(this, 'signal', new VixenAbortSignal(false), true); }
    abort(reason = undefined) { this.signal.__vixenAbortState.aborted = true; this.signal.__vixenAbortState.reason = reason; }
  }

  class VixenMutationObserver {
    constructor(callback) { defineReadonly(this, '__vixenCallback', callback, false); }
    observe() {}
    disconnect() {}
    takeRecords() { return []; }
  }

  function cloneValue(value, seen = new Map()) {
    if (value === null || typeof value !== 'object') return value;
    if (seen.has(value)) return seen.get(value);
    if (value instanceof Date) return new Date(value.getTime());
    if (value instanceof Map) {
      const out = new Map();
      seen.set(value, out);
      for (const [k, v] of value) out.set(cloneValue(k, seen), cloneValue(v, seen));
      return out;
    }
    if (value instanceof Set) {
      const out = new Set();
      seen.set(value, out);
      for (const item of value) out.add(cloneValue(item, seen));
      return out;
    }
    if (value instanceof Error) {
      const Ctor = value.constructor || Error;
      const out = new Ctor(value.message);
      out.name = value.name;
      return out;
    }
    if (Array.isArray(value)) {
      const out = [];
      seen.set(value, out);
      for (const item of value) out.push(cloneValue(item, seen));
      return out;
    }
    if (value instanceof ArrayBuffer) return value.slice(0);
    const out = {};
    seen.set(value, out);
    for (const [key, val] of Object.entries(value)) out[key] = cloneValue(val, seen);
    return out;
  }

  class VixenDOMParser {
    parseFromString(source, _type) { return new VixenParsedDocument(String(source)); }
  }

  class VixenParsedElement {
    constructor(tag, attrs, html) { this.localName = tag; this.tagName = tag.toUpperCase(); this.__vixenAttrs = attrs; this.innerHTML = html; }
    get id() { return this.__vixenAttrs.id || ''; }
    get textContent() { return this.innerHTML.replace(/<[^>]*>/g, ''); }
    getAttribute(name) { return Object.prototype.hasOwnProperty.call(this.__vixenAttrs, name) ? this.__vixenAttrs[name] : null; }
  }

  class VixenParsedDocument {
    constructor(source) { this.__vixenSource = source; }
    querySelector(selector) {
      const source = this.__vixenSource;
      if (String(selector).startsWith('#')) {
        const id = String(selector).slice(1).replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
        const re = new RegExp("<([A-Za-z][A-Za-z0-9-]*)([^>]*)\\bid=[\"']" + id + "[\"']([^>]*)>([\\s\\S]*?)<\\/\\1>", 'i');
        const m = source.match(re);
        if (!m) return null;
        return new VixenParsedElement(m[1].toLowerCase(), parseAttrs(m[2] + ' ' + m[3]), m[4]);
      }
      const tag = String(selector).toLowerCase();
      const re = new RegExp('<(' + tag + ')([^>]*)>([\\s\\S]*?)<\\/\\1>', 'i');
      const m = source.match(re);
      return m ? new VixenParsedElement(m[1].toLowerCase(), parseAttrs(m[2]), m[3]) : null;
    }
  }

  function parseAttrs(raw) {
    const attrs = {};
    raw.replace(/([A-Za-z_:][-A-Za-z0-9_:.]*)\s*=\s*(["'])(.*?)\2/g, (_m, name, _q, value) => { attrs[name] = value; return ''; });
    return attrs;
  }

  webidl.adoptInterface('AbortSignal', VixenAbortSignal);
  webidl.adoptInterface('AbortController', VixenAbortController);
  webidl.adoptInterface('MutationObserver', VixenMutationObserver);
  webidl.adoptInterface('DOMParser', VixenDOMParser);

  defineGlobal('structuredClone', cloneValue);

  const base64Alphabet = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';
  defineGlobal('btoa', (input) => {
    const bytes = Array.from(String(input), (ch) => ch.charCodeAt(0) & 0xff);
    let out = '';
    for (let i = 0; i < bytes.length; i += 3) {
      const n = (bytes[i] << 16) | ((bytes[i + 1] || 0) << 8) | (bytes[i + 2] || 0);
      out += base64Alphabet[(n >> 18) & 63] + base64Alphabet[(n >> 12) & 63] + (i + 1 < bytes.length ? base64Alphabet[(n >> 6) & 63] : '=') + (i + 2 < bytes.length ? base64Alphabet[n & 63] : '=');
    }
    return out;
  });
  defineGlobal('atob', (input) => {
    const clean = String(input).replace(/=+$/, '');
    let bits = 0, bitLength = 0, out = '';
    for (const ch of clean) {
      const value = base64Alphabet.indexOf(ch);
      if (value < 0) throw new TypeError('invalid base64');
      bits = (bits << 6) | value;
      bitLength += 6;
      if (bitLength >= 8) {
        bitLength -= 8;
        out += String.fromCharCode((bits >> bitLength) & 0xff);
      }
    }
    return out;
  });

  class VixenPerformance extends VixenEventTarget {
    get timeOrigin() { return startEpoch; }
    now() { return Math.max(0, Date.now() - startEpoch); }
    toJSON() { return { timeOrigin: this.timeOrigin }; }
    mark() {}
    measure() {}
    clearMarks() {}
    clearMeasures() {}
    getEntries() { return []; }
    getEntriesByName() { return []; }
    getEntriesByType() { return []; }
  }

  class VixenStorage {
    get length() { return 0; }
    key() { return null; }
    getItem() { return null; }
    setItem() {}
    removeItem() {}
    clear() {}
  }

  class VixenNavigator {
    get userAgent() { return 'Vixen/0.1'; }
    get language() { return 'en-US'; }
    get languages() { return ['en-US']; }
    get onLine() { return true; }
    get cookieEnabled() { return true; }
    get hardwareConcurrency() { return 1; }
    get maxTouchPoints() { return 0; }
    sendBeacon() { return false; }
    vibrate() { return false; }
  }

  function mediaMatches(query) {
    const q = String(query).toLowerCase();
    if (q.includes('print')) return false;
    if (q.includes('prefers-color-scheme: light')) return true;
    if (q.includes('orientation: landscape')) return true;
    const min = q.match(/min-width:\s*(\d+)px/);
    if (min && 800 < Number(min[1])) return false;
    const max = q.match(/max-width:\s*(\d+)px/);
    if (max && 800 > Number(max[1])) return false;
    return q.includes('screen') || q.includes('min-width') || q.includes('max-width') || q.trim() === 'all';
  }

  function matchMedia(query) {
    return { media: String(query), matches: mediaMatches(query), onchange: null, addEventListener() {}, removeEventListener() {}, dispatchEvent() { return true; } };
  }

  webidl.adoptInterface('Performance', VixenPerformance);
  webidl.adoptInterface('Storage', VixenStorage);
  webidl.adoptInterface('Navigator', VixenNavigator);

  if (typeof globalThis.window === 'undefined') defineGlobal('window', globalThis);
  if (typeof globalThis.self === 'undefined') defineGlobal('self', globalThis);
  defineGlobal('performance', new VixenPerformance());
  defineGlobal('navigator', new VixenNavigator());
  defineGlobal('localStorage', new VixenStorage());
  defineGlobal('sessionStorage', new VixenStorage());
  defineGlobal('history', { length: 1, state: null, scrollRestoration: 'auto', go() {}, back() {}, forward() {}, pushState() {}, replaceState() {} });
  defineGlobal('screen', { width: 800, height: 600, availWidth: 800, availHeight: 600, colorDepth: 24, pixelDepth: 24 });
  defineGlobal('visualViewport', { offsetLeft: 0, offsetTop: 0, pageLeft: 0, pageTop: 0, width: 800, height: 600, scale: 1 });
  defineGlobal('matchMedia', matchMedia);
  Object.defineProperties(globalThis, {
    innerWidth: { value: 800, writable: true, configurable: true },
    innerHeight: { value: 600, writable: true, configurable: true },
    devicePixelRatio: { value: 1, writable: true, configurable: true },
  });
})();
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn webapi_bootstrap_is_ascii_and_adopts_runtime_interfaces() {
        assert!(WEB_API_BOOTSTRAP.is_ascii());
        assert!(WEB_API_BOOTSTRAP.contains("adoptInterface('Event'"));
        assert!(WEB_API_BOOTSTRAP.contains("adoptInterface('DOMMatrix'"));
        assert!(WEB_API_BOOTSTRAP.contains("adoptInterface('Headers'"));
        assert!(WEB_API_BOOTSTRAP.contains("structuredClone"));
    }
}
