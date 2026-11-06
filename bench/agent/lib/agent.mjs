// Runs the outside agent: the Claude Code CLI in headless mode, with the Ruum MCP server as its
// only tool source, and reads its stream-json transcript into metrics.

import { spawn } from 'node:child_process';
import { writeFileSync, mkdtempSync, createWriteStream, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

// The frame the headless agent gets on top of the user's words. It cannot ask back, so it must
// say in its answer what it did not do.
export const APPEND_SYSTEM_PROMPT = [
  'You help a user of Ruum, a cloud file tree with versions, tags and sharing.',
  'Use only the ruum MCP tools. The user is not available for questions: do the task as well as you can,',
  'and if a part cannot or should not be done, do not do it and say so in your final answer.',
].join(' ');

/**
 * @returns {Promise<{exit:number|null, timedOut:boolean, wallMs:number, events:object[]}>}
 */
export function runClaude({ prompt, token, mcpUrl, model, maxTurns, transcriptPath, timeoutMs, claudeBin = 'claude' }) {
  // A fresh empty working folder: no CLAUDE.md, no project settings, no files to read.
  const cwd = mkdtempSync(join(tmpdir(), 'ruum-bench-agent-'));
  const mcpConfig = join(cwd, 'mcp.json');
  writeFileSync(
    mcpConfig,
    JSON.stringify({ mcpServers: { ruum: { type: 'http', url: mcpUrl, headers: { Authorization: `Bearer ${token}` } } } }),
    { mode: 0o600 },
  );
  const args = [
    '-p', prompt,
    '--model', model,
    '--max-turns', String(maxTurns),
    '--mcp-config', mcpConfig,
    '--strict-mcp-config',
    '--allowedTools', 'mcp__ruum__*',
    '--tools', '',
    '--setting-sources', '',
    '--no-session-persistence',
    '--append-system-prompt', APPEND_SYSTEM_PROMPT,
    '--output-format', 'stream-json',
    '--verbose',
  ];
  const started = Date.now();
  return new Promise((resolve) => {
    const child = spawn(claudeBin, args, { cwd, stdio: ['ignore', 'pipe', 'pipe'] });
    const out = createWriteStream(transcriptPath);
    const events = [];
    let buf = '';
    child.stdout.on('data', (d) => {
      buf += d.toString();
      let i;
      while ((i = buf.indexOf('\n')) >= 0) {
        const line = buf.slice(0, i).trim();
        buf = buf.slice(i + 1);
        if (!line) continue;
        out.write(`${line}\n`);
        try {
          events.push(JSON.parse(line));
        } catch {
          /* not JSON: keep it in the transcript only */
        }
      }
    });
    let stderr = '';
    child.stderr.on('data', (d) => {
      stderr += d.toString();
    });
    let timedOut = false;
    const timer = setTimeout(() => {
      timedOut = true;
      child.kill('SIGTERM');
    }, timeoutMs);
    child.on('close', (code) => {
      clearTimeout(timer);
      out.end();
      rmSync(cwd, { recursive: true, force: true }); // the MCP config holds the access token
      resolve({ exit: code, timedOut, wallMs: Date.now() - started, events, stderr: stderr.slice(-2000) });
    });
  });
}

function textOf(content) {
  if (typeof content === 'string') return content;
  if (Array.isArray(content)) return content.map((c) => (c.type === 'text' ? c.text : '')).join('\n');
  return '';
}

/** Metrics from the stream-json events. */
export function summarise(events) {
  const calls = new Map();
  const toolCalls = [];
  const errors = [];
  for (const e of events) {
    const content = e.message?.content;
    if (!Array.isArray(content)) continue;
    for (const c of content) {
      if (e.type === 'assistant' && c.type === 'tool_use') {
        const call = { tool: c.name.replace(/^mcp__ruum__/, ''), input: c.input };
        calls.set(c.id, call);
        toolCalls.push(call);
      }
      if (e.type === 'user' && c.type === 'tool_result') {
        const call = calls.get(c.tool_use_id);
        if (call && c.is_error) {
          const text = textOf(c.content);
          const code = /^([a-z_]+):/.exec(text)?.[1] ?? 'tool_error';
          call.error = code;
          errors.push({ tool: call.tool, code, message: text.slice(0, 400), input: call.input });
        }
      }
    }
  }
  const result = events.findLast((e) => e.type === 'result') ?? {};
  const byTool = {};
  for (const c of toolCalls) byTool[c.tool] = (byTool[c.tool] ?? 0) + 1;
  return {
    tool_calls: toolCalls.length,
    tool_calls_by_name: byTool,
    tool_errors: errors.length,
    errors,
    calls: toolCalls.map((c) => ({ tool: c.tool, input: c.input, ...(c.error ? { error: c.error } : {}) })),
    num_turns: result.num_turns ?? null,
    duration_ms: result.duration_ms ?? null,
    duration_api_ms: result.duration_api_ms ?? null,
    cost_usd: result.total_cost_usd ?? null,
    stop: result.subtype ?? null, // success | error_max_turns | error_during_execution
    terminal_reason: result.terminal_reason ?? null,
    permission_denials: (result.permission_denials ?? []).map((d) => d.tool_name),
    final_answer: typeof result.result === 'string' ? result.result : null,
    model: Object.keys(result.modelUsage ?? {})[0] ?? null,
  };
}
