/**
 * `@ruum/log`: structured logs for Ruum apps (design 9.6, docs/STEP3.md "Telemetry and privacy").
 *
 * Each call writes one JSON object on one line to stdout:
 *
 * ```json
 * {"time":"2026-11-19T10:00:00.000Z","severity_number":9,"severity_text":"INFO",
 *  "template":"order {orderId} paid","fields":{"orderId":"o-1"}}
 * ```
 *
 * The template is written as it is: no field value is put into it. So the platform classifies
 * each field by its name (`telemetry-privacy.yaml`), and the template text stays free of personal
 * values. Fields whose names are on the platform deny-list are written as the hidden marker
 * (defence in depth: the platform's processor still decides).
 */

import { HIDDEN, isDenied } from "./denylist.js";

export { HIDDEN, isDenied, segments, SEGMENTS, SUBSTRINGS } from "./denylist.js";

/** The log levels, with their OpenTelemetry severity numbers. */
export const LEVELS = Object.freeze({
  trace: 1,
  debug: 5,
  info: 9,
  warn: 13,
  error: 17,
  fatal: 21,
});

/** A log level name. */
export type Level = keyof typeof LEVELS;

/** The named fields of one log call. */
export type Fields = Record<string, unknown>;

/** Where log lines go: anything with a `write(string)` method, such as `process.stdout`. */
export interface Sink {
  write(line: string): unknown;
}

/** Options of {@link createLogger}. */
export interface LoggerOptions {
  /** The lowest level that is written. Default: `LOG_LEVEL` from the environment, else `info`. */
  level?: Level;
  /** Where lines go. Default: `process.stdout`. */
  sink?: Sink;
  /** Fields added to every line (for example `{ component: "billing" }`). */
  fields?: Fields;
  /** The clock (tests). Default: `() => new Date()`. */
  now?: () => Date;
}

/** A logger. Each method takes a message template and its named fields. */
export interface Logger {
  trace(template: string, fields?: Fields): void;
  debug(template: string, fields?: Fields): void;
  info(template: string, fields?: Fields): void;
  warn(template: string, fields?: Fields): void;
  error(template: string, fields?: Fields): void;
  fatal(template: string, fields?: Fields): void;
  /** A logger that adds `fields` to every line of this logger. */
  child(fields: Fields): Logger;
}

const MAX_DEPTH = 6;
const MAX_STRING = 8192;
/**
 * What a string longer than {@link MAX_STRING} becomes. The whole string is dropped, never cut:
 * the platform redacts a secret only when it sees the whole value (the runtime's log redactor and
 * the telemetry processor match exact values), so a cut string could carry most of a secret past
 * them (Kimi review of O-5, run 13, finding 1).
 */
export const TOO_LONG = "[too long]";
const MAX_KEYS = 100;

function isLevel(v: unknown): v is Level {
  return typeof v === "string" && Object.prototype.hasOwnProperty.call(LEVELS, v);
}

/**
 * Makes a value safe for JSON: hides denied keys at every depth, replaces long strings and deep
 * objects with markers, and turns values that JSON cannot hold into text.
 */
function clean(value: unknown, depth: number, seen: Set<object>): unknown {
  if (value === null) return null;
  switch (typeof value) {
    case "string":
      return value.length > MAX_STRING ? TOO_LONG : value;
    case "number":
      return Number.isFinite(value) ? value : String(value);
    case "boolean":
      return value;
    case "bigint":
      return value.toString();
    case "undefined":
      return null;
    case "symbol":
    case "function":
      return `[${typeof value}]`;
    default:
      break;
  }
  const obj = value as object;
  if (seen.has(obj)) return "[circular]";
  if (depth >= MAX_DEPTH) return "[too deep]";
  seen.add(obj);
  try {
    if (obj instanceof Date) {
      return Number.isNaN(obj.getTime()) ? "Invalid Date" : obj.toISOString();
    }
    if (obj instanceof Error) {
      // `name` and `message` can be set to any value (also an object with a denied key), and a
      // getter can throw: each one goes through clean.
      const read = (k: "name" | "message" | "stack"): unknown => {
        try {
          return obj[k];
        } catch {
          return "[unreadable]";
        }
      };
      const out: Record<string, unknown> = { name: clean(read("name"), depth + 1, seen), message: clean(read("message"), depth + 1, seen) };
      const stack = read("stack");
      if (typeof stack === "string") out["stack"] = clean(stack, depth + 1, seen);
      return out;
    }
    if (Array.isArray(obj)) {
      return obj.slice(0, MAX_KEYS).map((v) => clean(v, depth + 1, seen));
    }
    return cleanFields(obj as Record<string, unknown>, depth + 1, seen);
  } finally {
    seen.delete(obj);
  }
}

function cleanFields(fields: Record<string, unknown>, depth: number, seen: Set<object>): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  let n = 0;
  for (const key of Object.keys(fields)) {
    if (n++ >= MAX_KEYS) break;
    // A getter could throw; a denied key is never read at all.
    if (isDenied(key)) {
      Object.defineProperty(out, key, { value: HIDDEN, enumerable: true, writable: true, configurable: true });
      continue;
    }
    let v: unknown;
    try {
      v = fields[key];
    } catch {
      v = "[unreadable]";
    }
    // defineProperty keeps a field named `__proto__` as an own member of the line.
    Object.defineProperty(out, key, { value: clean(v, depth, seen), enumerable: true, writable: true, configurable: true });
  }
  return out;
}

/**
 * Makes a logger.
 *
 * @example
 * import { createLogger } from "@ruum/log";
 * const log = createLogger({ fields: { component: "billing" } });
 * log.info("order {orderId} paid", { orderId: order.id });
 */
export function createLogger(options: LoggerOptions = {}): Logger {
  const envLevel = process.env["LOG_LEVEL"]?.toLowerCase();
  const level: Level = options.level ?? (isLevel(envLevel) ? envLevel : "info");
  const min = LEVELS[level];
  const sink: Sink = options.sink ?? process.stdout;
  const now = options.now ?? (() => new Date());
  const base = options.fields ?? {};

  const write = (lvl: Level, template: string, fields?: Fields): void => {
    if (LEVELS[lvl] < min) return;
    const record = {
      time: now().toISOString(),
      severity_number: LEVELS[lvl],
      severity_text: lvl.toUpperCase(),
      template: typeof template === "string" ? clean(template, 0, new Set()) : String(template),
      // Each object is cleaned on its own: a spread would call the getter of a denied key.
      fields: { ...cleanFields(base, 1, new Set()), ...cleanFields(fields ?? {}, 1, new Set()) },
    };
    let line: string;
    try {
      line = JSON.stringify(record);
    } catch {
      line = JSON.stringify({ ...record, fields: {} });
    }
    sink.write(`${line}\n`);
  };

  return {
    trace: (t, f) => write("trace", t, f),
    debug: (t, f) => write("debug", t, f),
    info: (t, f) => write("info", t, f),
    warn: (t, f) => write("warn", t, f),
    error: (t, f) => write("error", t, f),
    fatal: (t, f) => write("fatal", t, f),
    child: (fields) => createLogger({ ...options, level, sink, now, fields: { ...base, ...fields } }),
  };
}

/**
 * The default logger: level from `LOG_LEVEL` (default `info`), lines to stdout.
 *
 * @example
 * import { log } from "@ruum/log";
 * log.warn("payment {paymentId} retried {attempt} times", { paymentId, attempt });
 */
export const log: Logger = createLogger();

export default log;
