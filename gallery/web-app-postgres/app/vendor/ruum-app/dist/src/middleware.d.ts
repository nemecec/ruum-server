/**
 * Helpers for Express and plain `node:http`: read `Ruum-User`, verify it, and put the user on the
 * request. Identity comes only from `Ruum-User`; no other header is ever trusted.
 */
import type { IncomingMessage, ServerResponse } from "node:http";
import { UserTokenVerifier, type RuumUser, type VerifierConfig } from "./verify.js";
/** A request with the verified user. */
export type RequestWithUser = IncomingMessage & {
    ruumUser?: RuumUser;
};
/** Options of {@link ruumUser} and {@link withRuumUser}. */
export interface MiddlewareOptions {
    /**
     * Whether visitors without sign-in are let in (a public app). The gateway still sends a token
     * (`sub: anonymous:<visit>`); a request without `Ruum-User` is always refused. Default `false`.
     */
    allowAnonymous?: boolean;
    /** The configuration. Default: {@link configFromEnv} (read once, when the middleware is made). */
    config?: VerifierConfig;
    /** A verifier to share between helpers. Default: one made from `config`. */
    verifier?: UserTokenVerifier;
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
export declare function ruumUser(options?: MiddlewareOptions): (req: IncomingMessage, res: ServerResponse, next: (err?: unknown) => void) => void;
/**
 * Wraps a plain `node:http` handler: the handler runs only with a verified user.
 *
 * @example
 * import http from "node:http";
 * import { withRuumUser } from "@ruum/app";
 * http.createServer(withRuumUser((req, res, user) => res.end(`hello ${user.sub}`))).listen(8080);
 */
export declare function withRuumUser(handler: (req: RequestWithUser, res: ServerResponse, user: RuumUser) => unknown, options?: MiddlewareOptions): (req: IncomingMessage, res: ServerResponse) => void;
