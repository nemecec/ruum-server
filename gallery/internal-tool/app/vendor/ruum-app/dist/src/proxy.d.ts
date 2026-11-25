/**
 * A small HTTPS GET that goes through the egress proxy when `HTTPS_PROXY` is set.
 *
 * Node 22's built-in `fetch` ignores `HTTPS_PROXY` unless the process starts with
 * `NODE_USE_ENV_PROXY=1` (or `--use-env-proxy`), which the SDK cannot rely on. So the SDK opens
 * the tunnel itself: an HTTP `CONNECT host:443` to the proxy, then TLS to the target inside the
 * tunnel with the normal certificate checks (`servername` = the target host).
 */
import tls from "node:tls";
/** Options of {@link httpsGet}. */
export interface HttpsGetOptions {
    /** The largest body accepted, in bytes. */
    maxBytes: number;
    /** The time limit of the whole request, in milliseconds. */
    timeoutMs: number;
    /** The proxy URL (`http://host:port`). Default: `HTTPS_PROXY` or `https_proxy`. */
    proxy?: string | undefined;
    /** Extra TLS options (tests: a private CA). */
    tlsOptions?: tls.ConnectionOptions;
}
/** The proxy from the environment, if any. */
export declare function proxyFromEnv(env?: NodeJS.ProcessEnv): string | undefined;
/** GETs an `https:` URL and returns the body as text, or throws on a status other than 200. */
export declare function httpsGet(url: string, options: HttpsGetOptions): Promise<string>;
