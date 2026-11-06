// Starts ruum-server against the laptop stack on the benchmark port, and runs `seed-dev`.

import { spawn, execFileSync } from 'node:child_process';
import { randomBytes } from 'node:crypto';
import { createWriteStream, readFileSync, existsSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';

/**
 * The checkout whose `scripts/dev.sh env` works: `deploy/dev/.env` is not in git, so a worktree
 * uses the main checkout of the same repository.
 */
export function stackRepo(repo) {
  if (existsSync(join(repo, 'deploy/dev/.env'))) return repo;
  const common = execFileSync('git', ['rev-parse', '--git-common-dir'], { cwd: repo }).toString().trim();
  const main = dirname(resolve(repo, common));
  if (existsSync(join(main, 'deploy/dev/.env'))) return main;
  throw new Error(`no deploy/dev/.env in ${repo} or ${main}; run scripts/dev.sh up`);
}

/** The environment of `scripts/dev.sh env`, plus the benchmark port and fresh signing keys. */
export function serverEnv(repo, port) {
  const out = execFileSync('bash', ['-c', 'e="$(scripts/dev.sh env)" && eval "$e" && env -0'], { cwd: stackRepo(repo) });
  const env = {};
  for (const line of out.toString().split('\0')) {
    const i = line.indexOf('=');
    if (i > 0) env[line.slice(0, i)] = line.slice(i + 1);
  }
  const base = `http://127.0.0.1:${port}`;
  return {
    ...env,
    RUUM_BIND: `127.0.0.1:${port}`,
    RUUM_PUBLIC_URL: base,
    RUUM_TOKEN_SIGNING_KEY: randomBytes(32).toString('base64url'),
    RUUM_CONTENT_SIGNING_KEY: randomBytes(32).toString('base64'),
    RUST_LOG: env.RUST_LOG ?? 'info',
  };
}

export async function startServer({ bin, env, port, logPath, cwd }) {
  const base = `http://127.0.0.1:${port}`;
  if (await healthy(base)) throw new Error(`port ${port} is in use`);
  const log = createWriteStream(logPath);
  const child = spawn(bin, [], { cwd, env, stdio: ['ignore', 'pipe', 'pipe'] }); // cwd: it reads migrations/
  child.stdout.pipe(log);
  child.stderr.pipe(log);
  for (let i = 0; i < 120; i++) {
    if (await healthy(base)) break;
    if (child.exitCode !== null) throw new Error(`the server stopped (see ${logPath})`);
    await new Promise((r) => setTimeout(r, 500));
  }
  if (!(await healthy(base))) throw new Error(`the server did not start (see ${logPath})`);
  const text = readFileSync(logPath, 'utf8');
  if (!text.includes('stores: Postgres') || !text.includes('authz engine: SpiceDB')) {
    child.kill();
    throw new Error(`the server does not use Postgres and SpiceDB (see ${logPath})`);
  }
  return {
    base,
    stop: () =>
      new Promise((r) => {
        if (child.exitCode !== null) return r();
        child.on('close', () => r());
        child.kill('SIGTERM');
      }),
  };
}

async function healthy(base) {
  try {
    return (await fetch(`${base}/healthz`)).ok;
  } catch {
    return false;
  }
}

/** Runs `ruum-server seed-dev` and returns its variables. */
export function seed({ bin, env, file, cwd }) {
  execFileSync(bin, ['seed-dev', file], { cwd, env, stdio: ['ignore', 'ignore', 'pipe'] });
  const vars = {};
  for (const line of readFileSync(file, 'utf8').split('\n')) {
    const m = /^([A-Z0-9_]+)='(.*)'$/.exec(line);
    if (m) vars[m[1]] = m[2];
  }
  return vars;
}
