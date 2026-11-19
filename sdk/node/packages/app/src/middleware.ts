/**
 * Helpers for Express and plain `node:http`: read `Ruum-User`, verify it, and put the user on the
 * request. Identity comes only from `Ruum-User`; no other header is ever trusted.
 */

import type { IncomingMessage, ServerResponse } from "node:http";
import { configFromEnv, RuumTokenError, UserTokenVerifier, type RuumUser, type VerifierConfig } from "./verify.js";

/** A request with the verified user. */
export type RequestWithUser = IncomingMessage & { ruumUser?: RuumUser };

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

function deny(res: ServerResponse, reason: string): void {
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

function headerToken(req: IncomingMessage): unknown {
  // Node joins repeated unknown headers with ", ", which is not a valid token: refused.
  const raw = req.headers["ruum-user"];
  return Array.isArray(raw) ? raw.join(", ") : raw;
}

/** Verifies the request's `Ruum-User`; returns the user, or `null` after it sent a 401. */
async function check(
  verifier: UserTokenVerifier,
  allowAnonymous: boolean,
  req: IncomingMessage,
  res: ServerResponse,
): Promise<RuumUser | null> {
  let user: RuumUser;
  try {
    user = await verifier.verify(headerToken(req));
  } catch (e) {
    deny(res, e instanceof RuumTokenError ? e.reason : "error");
    return null;
  }
  if (user.anonymous && !allowAnonymous) {
    deny(res, "anonymous");
    return null;
  }
  return user;
}

function verifierOf(options: MiddlewareOptions): UserTokenVerifier {
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
export function ruumUser(options: MiddlewareOptions = {}) {
  const verifier = verifierOf(options);
  const allowAnonymous = options.allowAnonymous ?? false;
  return (req: IncomingMessage, res: ServerResponse, next: (err?: unknown) => void): void => {
    check(verifier, allowAnonymous, req, res).then(
      (user) => {
        if (!user) return;
        (req as RequestWithUser).ruumUser = user;
        next();
      },
      (err: unknown) => next(err),
    );
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
export function withRuumUser(
  handler: (req: RequestWithUser, res: ServerResponse, user: RuumUser) => unknown,
  options: MiddlewareOptions = {},
): (req: IncomingMessage, res: ServerResponse) => void {
  const verifier = verifierOf(options);
  const allowAnonymous = options.allowAnonymous ?? false;
  return (req, res) => {
    check(verifier, allowAnonymous, req, res)
      .then(async (user) => {
        if (!user) return;
        (req as RequestWithUser).ruumUser = user;
        await handler(req as RequestWithUser, res, user);
      })
      .catch(() => {
        if (!res.headersSent) {
          res.statusCode = 500;
          res.end();
        } else {
          res.destroy();
        }
      });
  };
}
