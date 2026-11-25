/**
 * A strict JSON parser for token parts. `JSON.parse` keeps the last of two equal keys, so a token
 * with `"aud"` twice could mean one thing to the SDK and another to a different reader. This
 * parser refuses a duplicate key at any depth, and limits the depth.
 */
/** Thrown for text that is not strict JSON, has a duplicate key, or is too deep. */
export class StrictJsonError extends Error {
    kind;
    constructor(kind) {
        super(`strict JSON: ${kind}`);
        this.kind = kind;
        this.name = "StrictJsonError";
    }
}
const MAX_DEPTH = 32;
const NUMBER = /-?(?:0|[1-9]\d*)(?:\.\d+)?(?:[eE][+-]?\d+)?/y;
// A JSON string: no raw control characters, only the escapes JSON allows.
const STRING = /"(?:[^"\\\u0000-\u001f]|\\(?:["\\/bfnrt]|u[0-9a-fA-F]{4}))*"/y;
const WS = /[ \t\n\r]*/y;
/** Parses `text` as JSON and refuses duplicate object keys. */
export function parseStrictJson(text) {
    let i = 0;
    const ws = () => {
        WS.lastIndex = i;
        WS.exec(text);
        i = WS.lastIndex;
    };
    const str = () => {
        STRING.lastIndex = i;
        const m = STRING.exec(text);
        if (!m)
            throw new StrictJsonError("syntax");
        i = STRING.lastIndex;
        return JSON.parse(m[0]);
    };
    const value = (depth) => {
        if (depth > MAX_DEPTH)
            throw new StrictJsonError("too_deep");
        ws();
        const c = text[i];
        if (c === "{") {
            i++;
            const out = Object.create(null);
            const keys = new Set();
            ws();
            if (text[i] === "}") {
                i++;
                return out;
            }
            for (;;) {
                ws();
                const k = str();
                if (keys.has(k))
                    throw new StrictJsonError("duplicate_key");
                keys.add(k);
                ws();
                if (text[i] !== ":")
                    throw new StrictJsonError("syntax");
                i++;
                out[k] = value(depth + 1);
                ws();
                if (text[i] === ",") {
                    i++;
                    continue;
                }
                if (text[i] === "}") {
                    i++;
                    return out;
                }
                throw new StrictJsonError("syntax");
            }
        }
        if (c === "[") {
            i++;
            const out = [];
            ws();
            if (text[i] === "]") {
                i++;
                return out;
            }
            for (;;) {
                out.push(value(depth + 1));
                ws();
                if (text[i] === ",") {
                    i++;
                    continue;
                }
                if (text[i] === "]") {
                    i++;
                    return out;
                }
                throw new StrictJsonError("syntax");
            }
        }
        if (c === '"')
            return str();
        if (text.startsWith("true", i)) {
            i += 4;
            return true;
        }
        if (text.startsWith("false", i)) {
            i += 5;
            return false;
        }
        if (text.startsWith("null", i)) {
            i += 4;
            return null;
        }
        NUMBER.lastIndex = i;
        const m = NUMBER.exec(text);
        if (!m)
            throw new StrictJsonError("syntax");
        i = NUMBER.lastIndex;
        return Number(m[0]);
    };
    const v = value(0);
    ws();
    if (i !== text.length)
        throw new StrictJsonError("syntax");
    return v;
}
