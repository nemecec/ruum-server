/**
 * The platform deny-list: field names that are always `secret`. This is a copy of
 * `crates/ruum-core/src/telemetry/denylist.rs`; a test reads the Rust file and checks that the
 * two lists are equal, so they cannot drift.
 *
 * A name is split into segments: lower case, and `-`, `_`, `.`, spaces, `/`, `:` and camel-case
 * boundaries separate segments (`userPassword` gives `user`, `password`). The name is denied when
 * one segment is in {@link SEGMENTS}, or when the segments joined together contain one of
 * {@link SUBSTRINGS}. Over-matching is accepted: a denied name gives a hidden value, never a leak.
 */

/** Words that deny a name when the name without separators contains them. */
export const SUBSTRINGS: readonly string[] = Object.freeze([
  "password",
  "passwd",
  "passphrase",
  "secret",
  "token",
  "apikey",
  "authorization",
  "cookie",
  "credential",
  "privatekey",
  "accesskey",
  "session",
  "bearer",
  "signature",
  "connectionstring",
]);

/** Words that deny a name when one of its segments is exactly the word. */
export const SEGMENTS: readonly string[] = Object.freeze([
  "auth", "pwd", "pass", "jwt", "otp", "dsn", "csrf", "xsrf", "sid", "pin", "cvv", "cvc",
]);

/** The marker that replaces the value of a hidden field (the same text as the platform's). */
export const HIDDEN = "<hidden secret>";

const SEPARATORS = new Set(["-", "_", ".", " ", "/", ":"]);

function isUpper(c: string): boolean {
  return c !== c.toLowerCase() && c === c.toUpperCase();
}

function isLower(c: string): boolean {
  return c !== c.toUpperCase() && c === c.toLowerCase();
}

function isAsciiDigit(c: string): boolean {
  return c >= "0" && c <= "9";
}

/**
 * Splits a field name into lower-case segments, the same way as the platform.
 *
 * @example segments("XMLHttpToken") // ["xml", "http", "token"]
 */
export function segments(name: string): string[] {
  const chars = Array.from(name);
  const out: string[] = [];
  let cur = "";
  for (let i = 0; i < chars.length; i++) {
    const c = chars[i] as string;
    if (SEPARATORS.has(c)) {
      if (cur !== "") {
        out.push(cur);
        cur = "";
      }
      continue;
    }
    if (isUpper(c) && cur !== "") {
      const prev = chars[i - 1] as string;
      const next = chars[i + 1];
      const nextLower = next !== undefined && isLower(next);
      // `userPassword` (lower then upper) and `XMLHttp` (upper, upper then lower).
      if (isLower(prev) || isAsciiDigit(prev) || (isUpper(prev) && nextLower)) {
        out.push(cur);
        cur = "";
      }
    }
    cur += c.toLowerCase();
  }
  if (cur !== "") out.push(cur);
  return out;
}

/**
 * Whether a field name is on the platform deny-list (its value is always hidden).
 *
 * @example isDenied("userPassword") // true
 * @example isDenied("orderId") // false
 */
export function isDenied(name: string): boolean {
  const segs = segments(name);
  if (segs.some((s) => SEGMENTS.includes(s))) return true;
  const joined = segs.join("");
  return SUBSTRINGS.some((w) => joined.includes(w));
}
