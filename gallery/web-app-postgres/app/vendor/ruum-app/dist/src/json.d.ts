/**
 * A strict JSON parser for token parts. `JSON.parse` keeps the last of two equal keys, so a token
 * with `"aud"` twice could mean one thing to the SDK and another to a different reader. This
 * parser refuses a duplicate key at any depth, and limits the depth.
 */
/** Thrown for text that is not strict JSON, has a duplicate key, or is too deep. */
export declare class StrictJsonError extends Error {
    readonly kind: "syntax" | "duplicate_key" | "too_deep";
    constructor(kind: "syntax" | "duplicate_key" | "too_deep");
}
/** Parses `text` as JSON and refuses duplicate object keys. */
export declare function parseStrictJson(text: string): unknown;
