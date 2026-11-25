/**
 * Helpers for Express and plain `node:http`: read `Ruum-User`, verify it, and put the user on the
 * request. Identity comes only from `Ruum-User`; no other header is ever trusted.
 */
import { configFromEnv, RuumTokenError, UserTokenVerifier } from "./verify.js";
function deny(res, reason) {
    if (res.headersSent) {
        res.destroy();
        return;
    }
    const body = JSON.stringify({
        error: {
            code: "unauthenticated",
            message: "This request has no valid Ruum-User token.",
            hint: `The Ruum gateway sends the token with every request (reason: ${reason}).`,
        },
    });
    res.statusCode = 401;
    res.setHeader("content-type", "application/json");
    res.setHeader("cache-control", "no-store");
    res.end(body);
}
function headerToken(req) {
    // Node joins repeated unknown headers with ", ", which is not a valid token: refused.
    const raw = req.headers["ruum-user"];
    return Array.isArray(raw) ? raw.join(", ") : raw;
}
/** Verifies the request's `Ruum-User`; returns the user, or `null` after it sent a 401. */
async function check(verifier, allowAnonymous, req, res) {
    let user;
    try {
        user = await verifier.verify(headerToken(req));
    }
    catch (e) {
        deny(res, e instanceof RuumTokenError ? e.reason : "error");
        return null;
    }
    if (user.anonymous && !allowAnonymous) {
        deny(res, "anonymous");
        return null;
    }
    return user;
}
function verifierOf(options) {
    return options.verifier ?? new UserTokenVerifier(options.config ?? configFromEnv());
}
/**
 * Express (or Connect) middleware. Puts the user on `req.ruumUser`, or answers `401`.
 * The configuration is read when the middleware is made, so a missing `RUUM_ISSUER`,
 * `RUUM_AUDIENCE` or `RUUM_ENV` stops the app at start-up.
 *
 * @example
 * import express from "express";
 * import { ruumUser } from "@ruum/app";
 * const app = express();
 * app.use(ruumUser());
 * app.get("/", (req, res) => res.send(`hello ${req.ruumUser.sub}`));
 */
export function ruumUser(options = {}) {
    const verifier = verifierOf(options);
    const allowAnonymous = options.allowAnonymous ?? false;
    return (req, res, next) => {
        check(verifier, allowAnonymous, req, res).then((user) => {
            if (!user)
                return;
            req.ruumUser = user;
            next();
        }, (err) => next(err));
    };
}
/**
 * Wraps a plain `node:http` handler: the handler runs only with a verified user.
 *
 * @example
 * import http from "node:http";
 * import { withRuumUser } from "@ruum/app";
 * http.createServer(withRuumUser((req, res, user) => res.end(`hello ${user.sub}`))).listen(8080);
 */
export function withRuumUser(handler, options = {}) {
    const verifier = verifierOf(options);
    const allowAnonymous = options.allowAnonymous ?? false;
    return (req, res) => {
        check(verifier, allowAnonymous, req, res)
            .then(async (user) => {
            if (!user)
                return;
            req.ruumUser = user;
            await handler(req, res, user);
        })
            .catch(() => {
            if (!res.headersSent) {
                res.statusCode = 500;
                res.end();
            }
            else {
                res.destroy();
            }
        });
    };
}
