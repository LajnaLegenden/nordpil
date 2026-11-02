// The mongosh-style API for nordpil's shell. Runs once per run, before the
// script. Everything reaches the database through the native
// `__host(op, json) -> json`; values travel as JSON with BSON types wrapped
// in `{ "\0": <canonical extended JSON> }`.
(function (global) {
  'use strict';

  const host = global.__host;
  const TAG = '\u0000';
  const MAX_DEPTH = 100;
  const IS_DB = Symbol('database');
  const PATTERN = Symbol('pattern');

  function call(op, payload) {
    const text = host(op, JSON.stringify(encode(payload === undefined ? null : payload, 0)));
    return decode(JSON.parse(text));
  }

  // ---- BSON types -------------------------------------------------------

  function BSONValue() {}

  // Constructors work with and without `new`, like mongosh's.
  function define(name, constructor, methods) {
    constructor.prototype = Object.create(BSONValue.prototype);
    Object.assign(constructor.prototype, methods, { constructor });
    Object.defineProperty(constructor, 'name', { value: name });
    global[name] = constructor;
    return constructor;
  }

  const ObjectId = define('ObjectId', function ObjectId(hex) {
    if (!(this instanceof ObjectId)) return new ObjectId(hex);
    if (hex === undefined || hex === null) hex = call('objectId');
    else if (hex instanceof ObjectId) hex = hex.hex;
    if (typeof hex !== 'string' || !/^[0-9a-fA-F]{24}$/.test(hex)) {
      throw new TypeError("ObjectId takes 24 hex characters, e.g. ObjectId('507f1f77bcf86cd799439011').");
    }
    this.hex = hex.toLowerCase();
  }, {
    toString() { return this.hex; },
    toHexString() { return this.hex; },
    toJSON() { return this.hex; },
    equals(other) { return other instanceof ObjectId && other.hex === this.hex; },
    getTimestamp() { return new Date(parseInt(this.hex.slice(0, 8), 16) * 1000); },
    ejson() { return { $oid: this.hex }; },
  });
  ObjectId.isValid = (hex) => typeof hex === 'string' && /^[0-9a-fA-F]{24}$/.test(hex);
  ObjectId.createFromHexString = (hex) => new ObjectId(hex);

  const LONG_MIN = -(2n ** 63n);
  const LONG_MAX = 2n ** 63n - 1n;
  const Long = define('Long', function Long(value) {
    if (!(this instanceof Long)) return new Long(value);
    let big;
    try {
      big = value instanceof Long ? value.value
        : typeof value === 'number' ? BigInt(Math.trunc(value))
        : BigInt(String(value).trim());
    } catch (error) {
      throw new TypeError(`Long('${value}') is not a whole number.`);
    }
    if (big < LONG_MIN || big > LONG_MAX) throw new RangeError(`${value} doesn't fit in a Long.`);
    this.value = big;
  }, {
    toString() { return this.value.toString(); },
    toNumber() { return Number(this.value); },
    valueOf() { return Number(this.value); },
    toJSON() { return this.value.toString(); },
    equals(other) { return Long(other).value === this.value; },
    ejson() { return { $numberLong: this.value.toString() }; },
  });
  Long.fromNumber = (n) => new Long(n);
  Long.fromString = (text) => new Long(text);
  global.NumberLong = Long;

  const Int32 = define('Int32', function Int32(value) {
    if (!(this instanceof Int32)) return new Int32(value);
    const n = Number(value);
    if (!Number.isInteger(n) || n < -2147483648 || n > 2147483647) {
      throw new RangeError(`${value} is not a 32-bit integer.`);
    }
    this.value = n;
  }, {
    toString() { return String(this.value); },
    valueOf() { return this.value; },
    toJSON() { return this.value; },
    ejson() { return { $numberInt: String(this.value) }; },
  });
  global.NumberInt = Int32;

  const Double = define('Double', function Double(value) {
    if (!(this instanceof Double)) return new Double(value);
    this.value = Number(value);
  }, {
    toString() { return String(this.value); },
    valueOf() { return this.value; },
    toJSON() { return this.value; },
    ejson() { return { $numberDouble: numberText(this.value) }; },
  });

  const Decimal128 = define('Decimal128', function Decimal128(text) {
    if (!(this instanceof Decimal128)) return new Decimal128(text);
    this.text = String(text).trim();
  }, {
    toString() { return this.text; },
    toJSON() { return { $numberDecimal: this.text }; },
    ejson() { return { $numberDecimal: this.text }; },
  });
  Decimal128.fromString = (text) => new Decimal128(text);
  global.NumberDecimal = Decimal128;

  const Timestamp = define('Timestamp', function Timestamp(t, i) {
    if (!(this instanceof Timestamp)) return new Timestamp(t, i);
    if (t !== null && typeof t === 'object') ({ t, i } = t);
    this.t = Number(t ?? 0);
    this.i = Number(i ?? 0);
  }, {
    toString() { return `Timestamp({ t: ${this.t}, i: ${this.i} })`; },
    ejson() { return { $timestamp: { t: this.t, i: this.i } }; },
  });

  const BASE64 = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';

  function hexToBase64(hex) {
    let out = '';
    for (let index = 0; index < hex.length; index += 6) {
      const chunk = hex.slice(index, index + 6);
      const bits = parseInt(chunk.padEnd(6, '0'), 16);
      const chars = Math.ceil(chunk.length / 2 * 4 / 3);
      for (let k = 0; k < 4; k++) out += k < chars ? BASE64[(bits >> (18 - 6 * k)) & 63] : '=';
    }
    return out;
  }

  function base64ToHex(text) {
    let bits = 0;
    let count = 0;
    let out = '';
    for (const char of text.replace(/=+$/, '')) {
      const value = BASE64.indexOf(char);
      if (value < 0) continue;
      bits = (bits << 6) | value;
      count += 6;
      if (count >= 8) {
        count -= 8;
        out += ((bits >> count) & 255).toString(16).padStart(2, '0');
      }
    }
    return out;
  }

  const Binary = define('Binary', function Binary(base64, subType) {
    if (!(this instanceof Binary)) return new Binary(base64, subType);
    this.base64 = String(base64);
    this.subType = Number(subType ?? 0);
  }, {
    toString() {
      if (this.subType === 4) {
        const hex = base64ToHex(this.base64);
        return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
      }
      return this.base64;
    },
    toJSON() { return this.toString(); },
    hex() { return base64ToHex(this.base64); },
    ejson() {
      return { $binary: { base64: this.base64, subType: this.subType.toString(16).padStart(2, '0') } };
    },
  });
  global.BinData = (subType, base64) => new Binary(base64, subType);
  global.HexData = (subType, hex) => new Binary(hexToBase64(hex), subType);
  global.UUID = function UUID(text) {
    const hex = text === undefined ? call('uuid') : String(text).replace(/-/g, '');
    if (!/^[0-9a-fA-F]{32}$/.test(hex)) throw new TypeError(`UUID('${text}') needs 32 hex digits.`);
    return new Binary(hexToBase64(hex.toLowerCase()), 4);
  };

  const MinKey = define('MinKey', function MinKey() {
    if (!(this instanceof MinKey)) return new MinKey();
  }, { ejson() { return { $minKey: 1 }; } });
  const MaxKey = define('MaxKey', function MaxKey() {
    if (!(this instanceof MaxKey)) return new MaxKey();
  }, { ejson() { return { $maxKey: 1 }; } });

  // A regex JavaScript can't run, e.g. with the `x` option.
  const BSONRegExp = define('BSONRegExp', function BSONRegExp(pattern, options) {
    if (!(this instanceof BSONRegExp)) return new BSONRegExp(pattern, options);
    this.pattern = String(pattern);
    this.options = String(options ?? '');
  }, {
    toString() { return `/${this.pattern}/${this.options}`; },
    ejson() { return { $regularExpression: { pattern: this.pattern, options: this.options } }; },
  });

  // Symbols, code and DB pointers: kept as they came.
  const BSONOpaque = define('BSONOpaque', function BSONOpaque(ejson) {
    this.value = ejson;
  }, { ejson() { return this.value; } });

  global.ISODate = function ISODate(text) {
    if (text === undefined) return new Date();
    const date = new Date(text);
    if (Number.isNaN(date.getTime())) throw new TypeError(`ISODate('${text}') is not a valid date.`);
    return date;
  };

  // ---- Conversion -------------------------------------------------------

  function numberText(n) {
    if (Number.isNaN(n)) return 'NaN';
    if (n === Infinity) return 'Infinity';
    if (n === -Infinity) return '-Infinity';
    return Object.is(n, -0) ? '-0.0' : String(n);
  }

  function isInt32(n) {
    return Number.isInteger(n) && n >= -2147483648 && n <= 2147483647 && !Object.is(n, -0);
  }

  function regexOptions(flags) {
    return flags.split('').filter((flag) => 'imsux'.includes(flag)).sort().join('');
  }

  function encode(value, depth) {
    if (depth > MAX_DEPTH) throw new RangeError(`Values can nest at most ${MAX_DEPTH} levels.`);
    if (value === null || value === undefined) return null;
    switch (typeof value) {
      case 'string':
      case 'boolean':
        return value;
      case 'number':
        return isInt32(value) ? value : { [TAG]: { $numberDouble: numberText(value) } };
      case 'bigint':
        return { [TAG]: Long(value).ejson() };
      case 'function':
        throw new TypeError('A function can\'t be sent to the database.');
      case 'symbol':
        throw new TypeError('A symbol can\'t be sent to the database.');
    }
    if (value[IS_DB] === true) return value.getName();
    if (value instanceof BSONValue) return { [TAG]: value.ejson() };
    if (value instanceof Date) {
      const ms = value.getTime();
      if (Number.isNaN(ms)) throw new TypeError('An invalid date can\'t be sent to the database.');
      return { [TAG]: { $date: { $numberLong: String(ms) } } };
    }
    if (value instanceof RegExp) {
      const pattern = value[PATTERN] ?? value.source;
      return { [TAG]: { $regularExpression: { pattern, options: regexOptions(value.flags) } } };
    }
    if (value instanceof Cursor) {
      throw new TypeError('A cursor can\'t be sent to the database. Read it with toArray() first.');
    }
    if (value instanceof Collection) return value.getFullName();
    if (Array.isArray(value)) return value.map((item) => encode(item, depth + 1));
    const out = {};
    for (const key of Object.keys(value)) out[key] = encode(value[key], depth + 1);
    return out;
  }

  function fromEJSON(e) {
    if ('$oid' in e) return new ObjectId(e.$oid);
    if ('$numberLong' in e) return new Long(e.$numberLong);
    if ('$numberDouble' in e) return Number(e.$numberDouble);
    if ('$numberInt' in e) return Number(e.$numberInt);
    if ('$numberDecimal' in e) return new Decimal128(e.$numberDecimal);
    if ('$date' in e) return new Date(Number(e.$date.$numberLong));
    if ('$regularExpression' in e) {
      const { pattern, options } = e.$regularExpression;
      if (/^[imsu]*$/.test(options)) {
        try {
          const regex = new RegExp(pattern, options);
          // `source` escapes some patterns; send back what came.
          Object.defineProperty(regex, PATTERN, { value: pattern });
          return regex;
        } catch (error) { /* keep it as BSON */ }
      }
      return new BSONRegExp(pattern, options);
    }
    if ('$binary' in e) return new Binary(e.$binary.base64, parseInt(e.$binary.subType, 16));
    if ('$timestamp' in e) return new Timestamp(e.$timestamp);
    if ('$minKey' in e) return new MinKey();
    if ('$maxKey' in e) return new MaxKey();
    if ('$undefined' in e) return undefined;
    return new BSONOpaque(e);
  }

  function decode(value) {
    if (value === null || typeof value !== 'object') return value;
    if (Array.isArray(value)) {
      for (let index = 0; index < value.length; index++) value[index] = decode(value[index]);
      return value;
    }
    if (Object.prototype.hasOwnProperty.call(value, TAG)) return fromEJSON(value[TAG]);
    for (const key of Object.keys(value)) value[key] = decode(value[key]);
    return value;
  }

  // ---- Cursors, collections, databases -------------------------------------

  function Cursor(spec) {
    this._spec = spec;
    this._id = null;
    this._buffer = [];
    this._position = 0;
    this._done = false;
  }

  function cursorOption(name, key) {
    return function (value) {
      if (this._id !== null) throw new Error(`${name}() must come before the cursor is read.`);
      this._spec.options[key] = value;
      return this;
    };
  }

  Cursor.prototype = {
    constructor: Cursor,
    sort: cursorOption('sort', 'sort'),
    limit: cursorOption('limit', 'limit'),
    skip: cursorOption('skip', 'skip'),
    project: cursorOption('project', 'projection'),
    projection: cursorOption('projection', 'projection'),
    hint: cursorOption('hint', 'hint'),
    collation: cursorOption('collation', 'collation'),
    maxTimeMS: cursorOption('maxTimeMS', 'maxTimeMS'),
    allowDiskUse(value) { return cursorOption('allowDiskUse', 'allowDiskUse').call(this, value ?? true); },
    batchSize() { return this; },
    // True while documents are buffered or the host has more.
    _more() {
      if (this._position < this._buffer.length) return true;
      if (this._done) return false;
      if (this._id === null) this._id = call('open', this._spec);
      this._buffer = call('next', this._id);
      this._position = 0;
      if (this._buffer.length === 0) this._done = true;
      return !this._done;
    },
    hasNext() { return this._more(); },
    next() { return this._more() ? this._buffer[this._position++] : null; },
    tryNext() { return this.next(); },
    toArray() {
      const out = [];
      while (this._more()) {
        for (; this._position < this._buffer.length; this._position++) out.push(this._buffer[this._position]);
      }
      return out;
    },
    forEach(callback) {
      while (this._more()) {
        if (callback(this._buffer[this._position++]) === false) break;
      }
    },
    map(callback) { return this.toArray().map(callback); },
    itcount() {
      let count = 0;
      while (this._more()) {
        count += this._buffer.length - this._position;
        this._position = this._buffer.length;
      }
      return count;
    },
    close() {
      if (this._id !== null && !this._done) call('close', this._id);
      this._done = true;
      this._buffer = [];
    },
    isExhausted() { return this._done; },
    toString() { return 'Cursor'; },
    [Symbol.iterator]: function* () {
      while (this._more()) yield this._buffer[this._position++];
    },
  };

  function Collection(database, name) {
    this._database = database;
    this._name = name;
  }

  function write(collection, op, args) {
    return call('write', Object.assign({ ns: collection._ns(), op }, args, { options: args.options ?? {} }));
  }

  Collection.prototype = {
    constructor: Collection,
    getName() { return this._name; },
    getFullName() { return `${this._database}.${this._name}`; },
    toString() { return this.getFullName(); },
    _ns() { return { db: this._database, coll: this._name }; },
    find(filter, projection, options) {
      const all = Object.assign({}, options);
      if (projection !== undefined) all.projection = projection;
      return new Cursor({ kind: 'find', ns: this._ns(), filter: filter ?? {}, options: all });
    },
    findOne(filter, projection, options) {
      const cursor = this.find(filter, projection, options).limit(1);
      const document = cursor.next();
      cursor.close();
      return document;
    },
    aggregate(...args) {
      const [pipeline, options] = Array.isArray(args[0]) ? [args[0], args[1]] : [args, undefined];
      return new Cursor({ kind: 'aggregate', ns: this._ns(), pipeline, options: Object.assign({}, options) });
    },
    countDocuments(filter, options) {
      return call('count', { ns: this._ns(), filter: filter ?? {}, options: options ?? {} });
    },
    estimatedDocumentCount() { return call('estimatedCount', { ns: this._ns() }); },
    distinct(key, filter) {
      return call('distinct', { ns: this._ns(), key: String(key), filter: filter ?? {} });
    },
    insertOne(document, options) {
      return write(this, 'insertOne', { documents: [document], options });
    },
    insertMany(documents, options) {
      if (!Array.isArray(documents)) throw new TypeError('insertMany takes an array of documents.');
      return write(this, 'insertMany', { documents, options });
    },
    updateOne(filter, update, options) { return write(this, 'updateOne', { filter, update, options }); },
    updateMany(filter, update, options) { return write(this, 'updateMany', { filter, update, options }); },
    replaceOne(filter, replacement, options) {
      return write(this, 'replaceOne', { filter, update: replacement, options });
    },
    deleteOne(filter, options) { return write(this, 'deleteOne', { filter, options }); },
    deleteMany(filter, options) { return write(this, 'deleteMany', { filter, options }); },
    findOneAndUpdate(filter, update, options) {
      return write(this, 'findOneAndUpdate', { filter, update, options });
    },
    findOneAndReplace(filter, replacement, options) {
      return write(this, 'findOneAndReplace', { filter, update: replacement, options });
    },
    findOneAndDelete(filter, options) { return write(this, 'findOneAndDelete', { filter, options }); },
    bulkWrite(operations, options) {
      if (!Array.isArray(operations)) throw new TypeError('bulkWrite takes an array of operations.');
      return call('bulkWrite', { ns: this._ns(), operations, options: options ?? {} });
    },
  };

  function database(name) {
    const base = {
      [IS_DB]: true,
      getName() { return name; },
      getCollection(collection) {
        if (typeof collection !== 'string' || collection === '') {
          throw new TypeError('getCollection takes a collection name.');
        }
        return new Collection(name, collection);
      },
      toString() { return name; },
    };
    return new Proxy(base, {
      get(target, property) {
        if (typeof property === 'symbol' || property in target) return target[property];
        return new Collection(name, property);
      },
    });
  }

  // ---- Output and the hooks the rewritten script calls ---------------------

  // What print and results show: cursors read out, functions named.
  function printable(value) {
    if (value instanceof Cursor) return value.toArray();
    if (typeof value === 'function') return `[Function${value.name ? `: ${value.name}` : ''}]`;
    if (typeof value === 'symbol') return value.toString();
    return value;
  }

  global.print = (...values) => { call('print', { values: values.map(printable), json: false }); };
  global.printjson = (...values) => { call('print', { values: values.map(printable), json: true }); };
  global.console = { log: global.print, info: global.print, warn: global.print, error: global.print };

  global.__result = (index, value) => {
    if (value === undefined) return;
    if (value instanceof Cursor && value._id === null) {
      call('result', { index, cursor: value._spec });
      return;
    }
    call('result', { index, value: printable(value) });
  };

  global.__use = (name) => {
    global.db = database(name);
    print(`switched to db ${name}`);
  };

  global.__show = (what) => (what === 'dbs'
    ? call('listDatabases')
    : call('listCollections', global.db.getName()));

  global.__database = database;
}(globalThis));
