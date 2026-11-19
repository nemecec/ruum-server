#!/usr/bin/env node
/**
 * `ruum-dev-token`: dev mode on the command line.
 *
 *   ruum-dev-token init [--dir .ruum-dev]
 *   ruum-dev-token mint --aud app:<app-id>[/preview/<deployment-id>] [--env live|preview]
 *                       [--roles a,b] [--sub pairwise:dev-user] [--tenant dev] [--actor user]
 *                       [--ttl 60] [--issuer http://localhost] [--dir .ruum-dev]
 *
 * `init` makes a local key and prints the environment the app needs; `mint` prints one token.
 */

import { realpathSync } from "node:fs";
import { pathToFileURL } from "node:url";
import { DEV_DIR, DEV_ISSUER, DevModeError, initDev, mintDevToken } from "./dev.js";

function args(argv: string[]): Map<string, string> {
  const out = new Map<string, string>();
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i] as string;
    if (!a.startsWith("--")) throw new DevModeError(`unexpected argument ${JSON.stringify(a)}`);
    const v = argv[i + 1];
    if (v === undefined || v.startsWith("--")) throw new DevModeError(`${a} needs a value`);
    out.set(a.slice(2), v);
    i++;
  }
  return out;
}

const USAGE = `usage:
  ruum-dev-token init [--dir ${DEV_DIR}]
  ruum-dev-token mint --aud app:<app-id>[/preview/<deployment-id>] [--env live|preview] [--roles a,b]
                      [--sub pairwise:dev-user] [--tenant dev] [--actor user] [--ttl 60]
                      [--issuer ${DEV_ISSUER}] [--dir ${DEV_DIR}]`;

/** Runs the CLI; returns the exit status. */
export function main(argv: string[], out: (s: string) => void, err: (s: string) => void): number {
  const [cmd, ...rest] = argv;
  try {
    const a = args(rest);
    const dir = a.get("dir") ?? DEV_DIR;
    if (cmd === "init") {
      const f = initDev(dir);
      out(`# dev key in ${f.dir} (never commit it). Run your app with:\n`);
      out(`export RUUM_ISSUER=${a.get("issuer") ?? DEV_ISSUER}\n`);
      out(`export RUUM_APP_JWKS_FILE=${f.jwksFile}\n`);
      out(`export RUUM_ENV=preview  # or live, with a live audience; dev mode itself refuses RUUM_ENV=live\n`);
      out(`export RUUM_AUDIENCE=app:<app-id>/preview/<deployment-id>\n`);
      return 0;
    }
    if (cmd === "mint") {
      const aud = a.get("aud");
      if (!aud) throw new DevModeError("--aud is required");
      const roles = a.get("roles");
      const ttl = a.get("ttl");
      const token = mintDevToken(
        {
          audience: aud,
          ...(a.has("env") ? { env: a.get("env") as string } : {}),
          ...(roles !== undefined ? { roles: roles.split(",").map((r) => r.trim()).filter(Boolean) } : {}),
          ...(a.has("sub") ? { sub: a.get("sub") as string } : {}),
          ...(a.has("tenant") ? { tenant: a.get("tenant") as string } : {}),
          ...(a.has("actor") ? { actor: a.get("actor") as string } : {}),
          ...(a.has("issuer") ? { issuer: a.get("issuer") as string } : {}),
          ...(ttl !== undefined ? { lifetime: Number(ttl) } : {}),
        },
        dir,
      );
      out(`${token}\n`);
      return 0;
    }
    err(`${USAGE}\n`);
    return 2;
  } catch (e) {
    err(`ruum-dev-token: ${e instanceof DevModeError ? e.message : e instanceof Error ? e.name : "error"}\n`);
    return 1;
  }
}

function isMain(): boolean {
  const script = process.argv[1];
  if (script === undefined) return false;
  try {
    return import.meta.url === pathToFileURL(realpathSync(script)).href;
  } catch {
    return false;
  }
}

if (isMain()) {
  process.exitCode = main(process.argv.slice(2), (s) => process.stdout.write(s), (s) => process.stderr.write(s));
}
