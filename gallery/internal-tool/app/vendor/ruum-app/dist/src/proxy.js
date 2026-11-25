/**
 * A small HTTPS GET that goes through the egress proxy when `HTTPS_PROXY` is set.
 *
 * Node 22's built-in `fetch` ignores `HTTPS_PROXY` unless the process starts with
 * `NODE_USE_ENV_PROXY=1` (or `--use-env-proxy`), which the SDK cannot rely on. So the SDK opens
 * the tunnel itself: an HTTP `CONNECT host:443` to the proxy, then TLS to the target inside the
 * tunnel with the normal certificate checks (`servername` = the target host).
 */
import http from "node:http";
import https from "node:https";
import net from "node:net";
import tls from "node:tls";
/** The proxy from the environment, if any. */
export function proxyFromEnv(env = process.env) {
    const p = env["HTTPS_PROXY"] ?? env["https_proxy"];
    return p && p.trim() !== "" ? p.trim() : undefined;
}
/** GETs an `https:` URL and returns the body as text, or throws on a status other than 200. */
export function httpsGet(url, options) {
    const target = new URL(url);
    if (target.protocol !== "https:")
        return Promise.reject(new Error("only https: URLs are fetched"));
    if (target.username || target.password)
        return Promise.reject(new Error("a URL with credentials is refused"));
    const host = target.hostname.replace(/^\[|\]$/g, "");
    const port = Number(target.port || 443);
    const proxy = options.proxy ?? proxyFromEnv();
    return new Promise((resolve, reject) => {
        let done = false;
        const cleanups = [];
        const finish = (err, body) => {
            if (done)
                return;
            done = true;
            clearTimeout(timer);
            for (const c of cleanups)
                c();
            if (err)
                reject(err);
            else
                resolve(body);
        };
        const timer = setTimeout(() => finish(new Error("the request timed out")), options.timeoutMs);
        const request = (createConnection) => {
            const req = https.request({
                host,
                port,
                path: `${target.pathname}${target.search}`,
                method: "GET",
                servername: net.isIP(host) !== 0 ? undefined : host,
                headers: { accept: "application/json", host: target.host },
                agent: false,
                ...(createConnection ? { createConnection } : {}),
                ...(options.tlsOptions ?? {}),
            }, (res) => {
                if (res.statusCode !== 200) {
                    res.resume();
                    finish(new Error(`status ${res.statusCode}`));
                    return;
                }
                const chunks = [];
                let size = 0;
                res.on("data", (d) => {
                    size += d.length;
                    if (size > options.maxBytes) {
                        res.destroy();
                        finish(new Error("the response is too large"));
                        return;
                    }
                    chunks.push(d);
                });
                res.on("end", () => finish(null, Buffer.concat(chunks).toString("utf8")));
                res.on("error", (e) => finish(e));
            });
            cleanups.push(() => req.destroy());
            req.on("error", (e) => finish(e));
            req.end();
        };
        if (!proxy) {
            request();
            return;
        }
        let p;
        try {
            p = new URL(proxy);
        }
        catch {
            finish(new Error("HTTPS_PROXY is not a URL"));
            return;
        }
        if (p.protocol !== "http:") {
            finish(new Error("HTTPS_PROXY must be an http:// URL"));
            return;
        }
        const authority = host.includes(":") ? `[${host}]:${port}` : `${host}:${port}`;
        const connect = http.request({
            host: p.hostname.replace(/^\[|\]$/g, ""),
            port: Number(p.port || 80),
            method: "CONNECT",
            path: authority,
            headers: { host: authority },
            agent: false,
        });
        cleanups.push(() => connect.destroy());
        connect.on("connect", (res, socket) => {
            if (res.statusCode !== 200) {
                socket.destroy();
                finish(new Error(`the proxy refused the tunnel (status ${res.statusCode})`));
                return;
            }
            cleanups.push(() => socket.destroy());
            request(() => tls.connect({
                socket,
                servername: net.isIP(host) !== 0 ? undefined : host,
                ...(options.tlsOptions ?? {}),
            }));
        });
        connect.on("error", (e) => finish(e));
        connect.end();
    });
}
