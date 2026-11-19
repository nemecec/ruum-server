/**
 * Finds the secret names of the app's manifest (`secrets` in `app.yaml`), without a YAML
 * dependency. It reads the short form (a list of names) and the long form (a map whose keys are
 * the names), in block or flow style. Names follow the manifest schema: `^[A-Z][A-Z0-9_]{0,63}$`.
 */

import { existsSync, readFileSync, statSync } from "node:fs";
import { dirname, join, parse } from "node:path";

const NAME = /^[A-Z][A-Z0-9_]{0,63}$/;

function unquote(s: string): string {
  const t = s.trim();
  if ((t.startsWith('"') && t.endsWith('"')) || (t.startsWith("'") && t.endsWith("'"))) return t.slice(1, -1);
  return t;
}

/** The secret names in the text of an `app.yaml`. */
export function secretNamesFromManifest(text: string): string[] {
  const lines = text.split(/\r?\n/);
  const names: string[] = [];
  const add = (raw: string) => {
    const n = unquote(raw.replace(/\s+#.*$/, ""));
    if (NAME.test(n) && !names.includes(n)) names.push(n);
  };
  for (let i = 0; i < lines.length; i++) {
    const m = /^secrets\s*:\s*(.*)$/.exec(lines[i] as string);
    if (!m) continue;
    const rest = (m[1] as string).replace(/\s+#.*$/, "").trim();
    if (rest.startsWith("[") || rest.startsWith("{")) {
      // Flow style on one line: [A, B] or {A: {...}, B: {...}} (top-level names only).
      const inner = rest.slice(1, rest.lastIndexOf(rest.startsWith("[") ? "]" : "}"));
      let depth = 0;
      let cur = "";
      const parts: string[] = [];
      for (const ch of inner) {
        if (ch === "{" || ch === "[") depth++;
        if (ch === "}" || ch === "]") depth--;
        if (ch === "," && depth === 0) {
          parts.push(cur);
          cur = "";
        } else cur += ch;
      }
      parts.push(cur);
      for (const p of parts) add(rest.startsWith("[") ? p : (p.split(":")[0] ?? ""));
      continue;
    }
    // Block style: the lines indented under `secrets:`.
    let indent = -1;
    for (let j = i + 1; j < lines.length; j++) {
      const line = lines[j] as string;
      if (line.trim() === "" || line.trim().startsWith("#")) continue;
      const lead = (/^ */.exec(line) as RegExpExecArray)[0].length;
      if (lead === 0) break;
      if (indent === -1) indent = lead;
      if (lead !== indent) continue;
      const body = line.slice(lead);
      const item = /^-\s+(.+)$/.exec(body);
      if (item) add(item[1] as string);
      else {
        const key = /^([^:#]+):/.exec(body);
        if (key) add(key[1] as string);
      }
    }
  }
  return names;
}

const cache = new Map<string, string[]>();

/**
 * The secret names of the nearest `app.yaml` (or `app.yml`) at or above `startDir`, stopping at a
 * folder with `.git` or after 10 levels. Empty when none is found.
 */
export function findManifestSecrets(startDir: string): string[] {
  const hit = cache.get(startDir);
  if (hit) return hit;
  let dir = startDir;
  let found: string[] = [];
  for (let i = 0; i < 10; i++) {
    for (const name of ["app.yaml", "app.yml"]) {
      const f = join(dir, name);
      if (existsSync(f) && statSync(f).isFile()) {
        try {
          found = secretNamesFromManifest(readFileSync(f, "utf8"));
        } catch {
          found = [];
        }
        cache.set(startDir, found);
        return found;
      }
    }
    if (existsSync(join(dir, ".git")) || dir === parse(dir).root) break;
    dir = dirname(dir);
  }
  cache.set(startDir, found);
  return found;
}
