/**
 * The app's own credential for the runtime API: the workload token that the runtime mounts at
 * `RUUM_TOKEN_FILE` and renews (`contracts/apps/runtime.openapi.yaml`). The SDK reads the file on
 * every call and sends the token only to `ruum.internal` and its sub-domains (the runtime API and
 * the internal gateway); never to any other host.
 */
/** The default path of the workload token (the contract). */
export declare const DEFAULT_TOKEN_FILE = "/var/run/ruum/token";
/** Thrown when the workload token cannot be read or would go to a wrong host. */
export declare class WorkloadTokenError extends Error {
    constructor(message: string);
}
/**
 * Reads the workload token fresh from `RUUM_TOKEN_FILE` (default `/var/run/ruum/token`). Call it
 * for each request: the runtime renews the file before the token ends.
 *
 * @example
 * const token = readWorkloadToken();
 */
export declare function readWorkloadToken(env?: NodeJS.ProcessEnv): string;
/** Whether the workload token may go to `url`: `http(s)://ruum.internal` or a sub-domain of it. */
export declare function isRuumInternal(url: string | URL): boolean;
/**
 * The `Authorization` header for a request to `url`. Throws for any host other than
 * `ruum.internal`, so the token cannot leak to another server by mistake.
 *
 * @example
 * const res = await fetch("http://ruum.internal/v1/self", { headers: workloadAuthHeaders("http://ruum.internal/v1/self") });
 */
export declare function workloadAuthHeaders(url: string | URL, env?: NodeJS.ProcessEnv): Record<string, string>;
/**
 * `fetch` for the runtime API: adds the workload token. A path (`/v1/self`) is resolved against
 * `http://ruum.internal`. A URL on any other host is refused before anything is sent. Redirects are
 * not followed, so the token cannot be carried to another host.
 *
 * @example
 * const self = await (await runtimeFetch("/v1/self")).json();
 */
export declare function runtimeFetch(pathOrUrl: string | URL, init?: RequestInit, env?: NodeJS.ProcessEnv): Promise<Response>;
