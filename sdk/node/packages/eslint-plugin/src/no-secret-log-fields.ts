/**
 * The rule `ruum/no-secret-log-fields`. It reports a log call of `@ruum/log` (and `console.*`)
 * that logs a secret:
 *
 * - a field whose key is on the platform deny-list (`password`, `apiKey`, `access_token`, ...);
 * - a field whose key matches a secret name of the app's manifest (`secrets` in `app.yaml`):
 *   `stripeKey` matches `STRIPE_KEY`;
 * - a value that is `process.env` or `process.env.<SECRET>` (a manifest secret or a denied name).
 *
 * It follows spread objects, computed keys that are string literals or plain template literals,
 * template literals, string concatenation, `JSON.stringify(...)`, local variables with an object
 * or `process.env` value, and aliased imports (`import { log as l } from "@ruum/log"`).
 */

import type { Rule, Scope } from "eslint";
import type * as ESTree from "estree";
import { isDenied, segments } from "@ruum/log/denylist";
import { findManifestSecrets } from "./manifest.js";
import { dirname } from "node:path";

const LOG_METHODS = new Set(["trace", "debug", "info", "warn", "error", "fatal", "log"]);
const CONSOLE_METHODS = new Set(["log", "info", "warn", "error", "debug", "trace", "dir", "table"]);
const MAX_DEPTH = 6;

type Options = { secrets?: string[]; manifest?: boolean };

function norm(name: string): string {
  return segments(name).join("");
}

function propName(node: ESTree.Expression | ESTree.PrivateIdentifier | ESTree.Super, computed: boolean): string | null {
  if (!computed && node.type === "Identifier") return node.name;
  if (node.type === "Literal" && (typeof node.value === "string" || typeof node.value === "number")) return String(node.value);
  if (computed && node.type === "TemplateLiteral" && node.expressions.length === 0) {
    return node.quasis[0]?.value.cooked ?? null;
  }
  return null;
}

function isIdent(node: ESTree.Node, name: string): boolean {
  return node.type === "Identifier" && node.name === name;
}

/** `process.env` itself. */
function isProcessEnv(node: ESTree.Node): boolean {
  if (node.type !== "MemberExpression") return false;
  return isIdent(node.object, "process") && propName(node.property, node.computed) === "env";
}

const rule: Rule.RuleModule = {
  meta: {
    type: "problem",
    docs: {
      description: "Disallow secret fields and process.env values in log calls (Ruum apps).",
    },
    schema: [
      {
        type: "object",
        properties: {
          secrets: { type: "array", items: { type: "string" } },
          manifest: { type: "boolean" },
        },
        additionalProperties: false,
      },
    ],
    messages: {
      deniedField: "Do not log the field '{{name}}': its name is on the Ruum deny-list of secret names.",
      manifestSecret: "Do not log the field '{{name}}': it matches the secret {{secret}} of app.yaml.",
      processEnv: "Do not log process.env: it holds the app's secrets.",
      envSecret: "Do not log process.env.{{name}}: it is a secret.",
    },
  },

  create(context) {
    const opts = (context.options[0] ?? {}) as Options;
    const secrets = new Set<string>(opts.secrets ?? []);
    if (opts.manifest !== false) {
      const file = context.physicalFilename ?? context.filename;
      if (file && !file.startsWith("<")) {
        for (const s of findManifestSecrets(dirname(file))) secrets.add(s);
      }
    }
    const secretNorms = new Map<string, string>();
    for (const s of secrets) secretNorms.set(norm(s), s);

    const loggers = new Set<string>();
    const factories = new Set<string>();
    const namespaces = new Set<string>();

    const manifestSecretOf = (name: string): string | undefined => secretNorms.get(norm(name));

    const isSecretEnvName = (name: string): boolean =>
      isDenied(name) || manifestSecretOf(name) !== undefined || name.startsWith("RUUM_SECRET_");

    /** Whether `node` is a logger value: an imported logger, a namespace's `log`, or `x.child(...)`. */
    const isLogger = (node: ESTree.Node): boolean => {
      if (node.type === "Identifier") return loggers.has(node.name);
      if (node.type === "MemberExpression" && node.object.type === "Identifier" && namespaces.has(node.object.name)) {
        const p = propName(node.property, node.computed);
        return p === "log" || p === "default";
      }
      if (node.type === "CallExpression" && node.callee.type === "MemberExpression") {
        return propName(node.callee.property, node.callee.computed) === "child" && isLogger(node.callee.object);
      }
      return false;
    };

    const isFactoryCall = (node: ESTree.Node): boolean => {
      if (node.type !== "CallExpression") return false;
      const c = node.callee;
      if (c.type === "Identifier") return factories.has(c.name);
      if (c.type === "MemberExpression") {
        if (c.object.type === "Identifier" && namespaces.has(c.object.name)) {
          return propName(c.property, c.computed) === "createLogger";
        }
        return propName(c.property, c.computed) === "child" && isLogger(c.object);
      }
      return false;
    };

    /** The value a local variable was given, if it has one definition with an initial value. */
    const initOf = (id: ESTree.Identifier): ESTree.Expression | null => {
      let scope: Scope.Scope | null = context.sourceCode.getScope(id);
      while (scope) {
        const v = scope.set.get(id.name);
        if (v) {
          const def = v.defs[0];
          if (v.defs.length === 1 && def && def.type === "Variable" && def.node.id.type === "Identifier") {
            return def.node.init ?? null;
          }
          return null;
        }
        scope = scope.upper;
      }
      return null;
    };

    const check = (node: ESTree.Node | null | undefined, depth: number): void => {
      if (!node || depth > MAX_DEPTH) return;
      switch (node.type) {
        case "ObjectExpression":
          for (const p of node.properties) {
            if (p.type === "SpreadElement") {
              check(p.argument, depth + 1);
              continue;
            }
            const name = propName(p.key, p.computed);
            if (name !== null) {
              const secret = manifestSecretOf(name);
              if (isDenied(name)) {
                context.report({ node: p.key, messageId: "deniedField", data: { name } });
              } else if (secret !== undefined) {
                context.report({ node: p.key, messageId: "manifestSecret", data: { name, secret } });
              }
            }
            check(p.value, depth + 1);
          }
          return;
        case "MemberExpression": {
          if (isProcessEnv(node)) {
            context.report({ node, messageId: "processEnv" });
            return;
          }
          if (isProcessEnv(node.object)) {
            const name = propName(node.property, node.computed);
            if (name !== null && isSecretEnvName(name)) {
              context.report({ node, messageId: "envSecret", data: { name } });
            }
          }
          return;
        }
        case "Identifier": {
          const init = initOf(node);
          if (init) check(init, depth + 1);
          return;
        }
        case "TemplateLiteral":
          for (const e of node.expressions) check(e, depth + 1);
          return;
        case "ArrayExpression":
          for (const e of node.elements) check(e?.type === "SpreadElement" ? e.argument : e, depth + 1);
          return;
        case "SpreadElement":
          check(node.argument, depth + 1);
          return;
        case "ConditionalExpression":
          check(node.consequent, depth + 1);
          check(node.alternate, depth + 1);
          return;
        case "LogicalExpression":
        case "BinaryExpression":
          check(node.left, depth + 1);
          check(node.right, depth + 1);
          return;
        case "AwaitExpression":
          check(node.argument, depth + 1);
          return;
        case "CallExpression": {
          const c = node.callee;
          const fn =
            c.type === "Identifier"
              ? c.name
              : c.type === "MemberExpression" && c.object.type === "Identifier"
                ? `${c.object.name}.${propName(c.property, c.computed) ?? ""}`
                : "";
          if (fn === "JSON.stringify" || fn === "String" || fn === "Object.assign" || fn === "Object.entries" || fn === "Object.values") {
            for (const a of node.arguments) check(a, depth + 1);
          }
          return;
        }
        default:
          return;
      }
    };

    return {
      ImportDeclaration(node) {
        if (node.source.value !== "@ruum/log") return;
        for (const s of node.specifiers) {
          if (s.type === "ImportDefaultSpecifier") loggers.add(s.local.name);
          else if (s.type === "ImportNamespaceSpecifier") namespaces.add(s.local.name);
          else {
            const imported = s.imported.type === "Identifier" ? s.imported.name : String(s.imported.value);
            if (imported === "log" || imported === "default") loggers.add(s.local.name);
            if (imported === "createLogger") factories.add(s.local.name);
          }
        }
      },
      VariableDeclarator(node) {
        if (node.id.type === "Identifier" && node.init && (isFactoryCall(node.init) || isLogger(node.init))) {
          loggers.add(node.id.name);
        }
      },
      CallExpression(node) {
        // createLogger({ fields }) and logger.child(fields) carry fields too.
        if (isFactoryCall(node)) {
          for (const a of node.arguments) check(a, 0);
          return;
        }
        const c = node.callee;
        if (c.type !== "MemberExpression") return;
        const method = propName(c.property, c.computed);
        if (method === null) return;
        const isConsole = isIdent(c.object, "console") && CONSOLE_METHODS.has(method);
        const isRuum = LOG_METHODS.has(method) && isLogger(c.object);
        if (!isConsole && !isRuum) return;
        for (const a of node.arguments) check(a, 0);
      },
    };
  },
};

export default rule;
