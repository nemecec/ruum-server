#!/usr/bin/env node
// The agent benchmark of Ruum (docs/BENCHMARK.md, design sections 18.7 and 18.9).
//
//   node run.mjs                      # every task, the outside agent (claude -p, sonnet)
//   node run.mjs --mode oracle        # the reference solutions instead of the agent
//   node run.mjs --mode null          # no action at all (each normal task must then fail)
//   node run.mjs --only tag_pdfs,share_view
//   node run.mjs --mode smoke --only share_view   # one get_me call over MCP, to test the wiring
//
// It starts ruum-server against the laptop stack on port 8494, and for each task: seeds a fresh
// tenant (`ruum-server seed-dev`), runs the setup through the REST API, registers a benchmark
// agent for the task's persona, gives the prompt to the outside agent over MCP, and runs the
// checker on the final state through the REST API. Results go to results/<run>.json, and the
// agent transcripts to results/<run>/<task>.jsonl.

import { readdirSync, readFileSync, writeFileSync, mkdirSync, mkdtempSync, rmSync } from 'node:fs';
import { join, dirname, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';
import { parseArgs } from 'node:util';
import YAML from 'yaml';
import { Api } from './lib/api.mjs';
import { runStep, template } from './lib/steps.mjs';
import { runChecks } from './lib/checks.mjs';
import { snapshot, outcomeOf, gate } from './lib/outcome.mjs';
import { runClaude, summarise, APPEND_SYSTEM_PROMPT } from './lib/agent.mjs';
import { serverEnv, startServer, seed } from './lib/server.mjs';

const HERE = dirname(fileURLToPath(import.meta.url));

const { values: opt } = parseArgs({
  options: {
    mode: { type: 'string', default: 'agent' },
    only: { type: 'string' },
    port: { type: 'string', default: '8494' },
    model: { type: 'string', default: 'sonnet' },
    'max-turns': { type: 'string', default: '25' },
    'server-bin': { type: 'string' },
    repo: { type: 'string', default: resolve(HERE, '../..') },
    run: { type: 'string' },
    'timeout-min': { type: 'string', default: '14' },
  },
});
if (!['agent', 'oracle', 'null', 'smoke'].includes(opt.mode)) throw new Error(`--mode agent|oracle|null|smoke, not ${opt.mode}`);
// smoke: checks the MCP connection of the outside agent with one harmless prompt (no results).
const SMOKE_PROMPT = 'Call the get_me tool once and reply with your agent name only.';

const REQUIRED = ['id', 'title', 'kind', 'persona', 'prompt', 'setup', 'check'];

function loadTasks() {
  const dir = join(HERE, 'tasks');
  const only = opt.only ? opt.only.split(',') : null;
  return readdirSync(dir)
    .filter((f) => f.endsWith('.yaml'))
    .sort()
    .map((f) => ({ file: f, ...YAML.parse(readFileSync(join(dir, f), 'utf8')) }))
    .filter((t) => {
      for (const k of REQUIRED) if (!(k in t)) throw new Error(`${t.file}: missing ${k}`);
      if (!['normal', 'trap', 'refusal'].includes(t.kind)) throw new Error(`${t.file}: kind normal|trap|refusal`);
      // A comma inside a plain flow-map value splits it into a key with no value: refuse that.
      for (const s of [...t.setup, ...t.check, ...(t.reference ?? [])]) {
        for (const [k, v] of Object.entries(s)) if (v === null) throw new Error(`${t.file}: key "${k}" has no value (quote strings with commas)`);
      }
      return !only || only.includes(t.id);
    });
}

async function runTask(task, server, env, work, runDir) {
  const started = Date.now();
  const seedFile = join(work, `${task.id}.seed.env`);
  const vars = seed({ bin: opt['server-bin'], env, file: seedFile, cwd: opt.repo });
  rmSync(seedFile, { force: true }); // it holds tokens
  vars.BOB_EMAIL = `bob-${vars.SEED_RUN}@example.test`;
  vars.CAROL_EMAIL = `carol-${vars.SEED_RUN}@example.test`;
  const alice = new Api(server.base, vars.ALICE_TOKEN, vars.TENANT_A);
  const ctx = {
    base: server.base,
    vars,
    inspector: alice,
    principals: {
      alice,
      bob: new Api(server.base, vars.BOB_TOKEN, vars.TENANT_A),
      carol: new Api(server.base, vars.CAROL_TOKEN, vars.TENANT_B),
    },
    agents: {},
    saved: {},
  };
  // The seed wrote its SpiceDB tuples from another process: wait for the quantization window.
  await new Promise((r) => setTimeout(r, 6000));

  const record = { id: task.id, title: task.title, kind: task.kind, persona: task.persona };
  try {
    for (const step of task.setup) await runStep(ctx, step);
    await runStep(ctx, {
      agent: 'Benchmark agent',
      as: task.persona,
      scope: task.agent_scope ?? {},
      description: 'outside agent of the benchmark',
      save: 'bench',
    });
  } catch (e) {
    return { ...record, success: false, outcome: 'fail', setup_error: e.message, wall_ms: Date.now() - started };
  }
  const setupMs = Date.now() - started;
  // What the user's tenant looks like before the agent: a safe stop leaves it unchanged.
  const before = await snapshot(alice);
  const prompt = opt.mode === 'smoke' ? SMOKE_PROMPT : template(task.prompt, vars).trim();
  record.prompt = prompt;

  let agent = null;
  if (opt.mode === 'agent' || opt.mode === 'smoke') {
    const transcript = join(runDir, `${task.id}.jsonl`);
    const run = await runClaude({
      prompt,
      token: ctx.principals.bench.token,
      mcpUrl: `${server.base}/mcp`,
      model: opt.model,
      maxTurns: Number(opt['max-turns']),
      transcriptPath: transcript,
      timeoutMs: Number(opt['timeout-min']) * 60_000,
    });
    agent = { exit: run.exit, timed_out: run.timedOut, wall_ms: run.wallMs, ...summarise(run.events) };
    if (run.exit !== 0 && run.stderr) agent.stderr = run.stderr;
  } else if (opt.mode === 'oracle') {
    try {
      for (const step of task.reference ?? []) await runStep(ctx, { as: 'bench', ...step });
    } catch (e) {
      record.reference_error = e.message;
    }
  }

  const checked = await runChecks(ctx, task.check);
  let answer = null;
  if (task.answer_should_match && agent?.final_answer != null) {
    answer = new RegExp(task.answer_should_match, 'i').test(agent.final_answer);
  }
  const after = await snapshot(alice);
  const outcome = outcomeOf({
    success: checked.success,
    checks: checked.checks,
    before,
    after,
    answer: agent?.final_answer ?? null,
    safeStopAnswer: task.safe_stop_answer,
  });
  return {
    ...record,
    success: checked.success,
    outcome,
    unchanged: JSON.stringify(before) === JSON.stringify(after),
    checks: checked.checks,
    answer_matches: answer,
    setup_ms: setupMs,
    wall_ms: Date.now() - started,
    agent,
  };
}

function table(results) {
  const rows = ['| Task | Kind | Outcome | Turns | Tool calls | Tool errors | Agent time (s) | Cost (USD) |', '|---|---|---|---|---|---|---|---|'];
  for (const r of results) {
    const a = r.agent ?? {};
    const sec = a.wall_ms != null ? (a.wall_ms / 1000).toFixed(0) : '-';
    rows.push(
      `| ${r.id} | ${r.kind} | ${r.outcome} | ${a.num_turns ?? '-'} | ${a.tool_calls ?? '-'} | ${a.tool_errors ?? '-'} | ${sec} | ${a.cost_usd != null ? a.cost_usd.toFixed(2) : '-'} |`,
    );
  }
  return rows.join('\n');
}

async function main() {
  const tasks = loadTasks();
  if (!opt['server-bin']) opt['server-bin'] = join(opt.repo, 'target/debug/ruum-server');
  const stamp = new Date().toISOString().replace(/[:.]/g, '-').slice(0, 19);
  const runName = opt.run ?? `${stamp}-${opt.mode}`;
  const resultsDir = join(HERE, 'results');
  const runDir = join(resultsDir, runName);
  mkdirSync(runDir, { recursive: true });
  const work = mkdtempSync(join(tmpdir(), 'ruum-bench-'));
  const port = Number(opt.port);
  const env = serverEnv(opt.repo, port);
  console.log(`run ${runName}: ${tasks.length} tasks, mode ${opt.mode}, server log ${work}/server.log`);
  const server = await startServer({ bin: opt['server-bin'], env, port, logPath: join(work, 'server.log'), cwd: opt.repo });
  const results = [];
  try {
    for (const task of tasks) {
      process.stdout.write(`- ${task.id} ... `);
      const r = await runTask(task, server, env, work, runDir);
      results.push(r);
      const a = r.agent;
      console.log(
        `${r.outcome.toUpperCase()}${r.setup_error ? ` (setup: ${r.setup_error})` : ''}` +
          (a ? ` turns=${a.num_turns} calls=${a.tool_calls} errors=${a.tool_errors} ${(a.wall_ms / 1000).toFixed(0)}s` : ''),
      );
      for (const c of r.checks ?? []) if (!c.ok) console.log(`    x ${c.harm ? '(harm) ' : ''}${c.what}: ${c.detail}`);
    }
  } finally {
    await server.stop();
  }
  const report = {
    run: runName,
    mode: opt.mode,
    started_at: stamp,
    model: opt.model,
    max_turns: Number(opt['max-turns']),
    agent_cli: opt.mode === 'agent' ? 'claude -p --output-format stream-json --verbose --tools "" --strict-mcp-config --allowedTools mcp__ruum__*' : null,
    append_system_prompt: APPEND_SYSTEM_PROMPT,
    passed: results.filter((r) => r.success).length,
    total: results.length,
    gate: gate(results),
    results,
  };
  const out = join(resultsDir, `${runName}.json`);
  writeFileSync(out, `${JSON.stringify(report, null, 2)}\n`);
  if (opt.mode !== 'agent') rmSync(runDir, { recursive: true, force: true });
  const g = report.gate;
  console.log(
    `\n${table(results)}\n\npassed ${g.passed} of ${g.total} (${(g.pass_rate * 100).toFixed(0)} %), ` +
      `safe stops ${g.safe_stops}, failed ${g.failed}, harmful ${g.harmful}${g.harmful_ok ? '' : ' (the gate needs 0)'}; ` +
      `results: ${out}; server log: ${work}/server.log`,
  );
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
