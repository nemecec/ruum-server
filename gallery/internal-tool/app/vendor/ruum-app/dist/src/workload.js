/**
 * The app's own credential for the runtime API: the workload token that the runtime mounts at
 * `RUUM_TOKEN_FILE` and renews (`contracts/apps/runtime.openapi.yaml`). The SDK reads the file on
 * every call and sends the token only to `ruum.internal` and its sub-domains (the runtime API and
 * the internal gateway); never to any other host.
 */
import { readFileSync } from "node:fs";
/** The default path of the workload token (the contract). */
export const DEFAULT_TOKEN_FILE = "/var/run/ruum/token";
const MAX_TOKEN_BYTES = 16 * 1024;
/** Thrown when the workload token cannot be read or would go to a wrong host. */
export class WorkloadTokenError extends Error {
    constructor(message) {
        super(message);
        this.name = "WorkloadTokenError";
    }
}
/**
 * Reads the workload token fresh from `RUUM_TOKEN_FILE` (default `/var/run/ruum/token`). Call it
 * for each request: the runtime renews the file before the token ends.
 *
 * @example
 * const token = readWorkloadToken();
 */
export function readWorkloadToken(env = process.env) {
    const file = env["RUUM_TOKEN_FILE"]?.trim() || DEFAULT_TOKEN_FILE;
    let text;
    try {
        text = readFileSync(file, "utf8");
    }
    catch (e) {
        const code = e.code ?? "error";
        throw new WorkloadTokenError(`the workload token file cannot be read (${code})`);
    }
    const token = text.trim();
    if (token === "" || Buffer.byteLength(token) > MAX_TOKEN_BYTES || /[\s,]/.test(token)) {
        throw new WorkloadTokenError("the workload token file does not hold one token");
    }
    return token;
}
/** Whether the workload token may go to `url`: `http(s)://ruum.internal` or a sub-domain of it. */
export function isRuumInternal(url) {
    let u;
    try {
        u = new URL(url);
    }
    catch {
        return false;
    }
    if (u.protocol !== "http:" && u.protocol !== "https:")
        return false;
    if (u.username !== "" || u.password !== "")
        return false;
    const host = u.hostname.toLowerCase();
    return host === "ruum.internal" || host.endsWith(".ruum.internal");
}
/**
 * The `Authorization` header for a request to `url`. Throws for any host other than
 * `ruum.internal`, so the token cannot leak to another server by mistake.
 *
 * @example
 * const res = await fetch("http://ruum.internal/v1/self", { headers: workloadAuthHeaders("http://ruum.internal/v1/self") });
 */
export function workloadAuthHeaders(url, env = process.env) {
    if (!isRuumInternal(url)) {
        throw new WorkloadTokenError("the workload token is sent only to ruum.internal");
    }
    return { authorization: `Bearer ${readWorkloadToken(env)}` };
}
/**
 * `fetch` for the runtime API: adds the workload token. A path (`/v1/self`) is resolved against
 * `http://ruum.internal`. A URL on any other host is refused before anything is sent. Redirects are
 * not followed, so the token cannot be carried to another host.
 *
 * @example
 * const self = await (await runtimeFetch("/v1/self")).json();
 */
export async function runtimeFetch(pathOrUrl, init = {}, env = process.env) {
    const url = new URL(pathOrUrl, "http://ruum.internal");
    const headers = new Headers(init.headers);
    for (const [k, v] of Object.entries(workloadAuthHeaders(url, env)))
        headers.set(k, v);
    return fetch(url, { ...init, headers, redirect: "manual" });
}
