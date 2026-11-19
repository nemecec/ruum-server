/**
 * `@ruum/eslint-plugin`: lint rules for Ruum apps.
 *
 * @example
 * // eslint.config.js
 * import ruum from "@ruum/eslint-plugin";
 * export default [{ plugins: { ruum }, rules: { "ruum/no-secret-log-fields": "error" } }];
 */

import noSecretLogFields from "./no-secret-log-fields.js";

export { secretNamesFromManifest, findManifestSecrets } from "./manifest.js";

const plugin = {
  meta: { name: "@ruum/eslint-plugin", version: "0.1.0" },
  rules: { "no-secret-log-fields": noSecretLogFields },
  configs: {} as Record<string, unknown>,
};

/** The recommended flat config: `ruum/no-secret-log-fields` as an error. */
plugin.configs["recommended"] = {
  plugins: { ruum: plugin },
  rules: { "ruum/no-secret-log-fields": "error" },
};

export default plugin;
