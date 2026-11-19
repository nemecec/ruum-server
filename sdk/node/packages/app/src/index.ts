/**
 * `@ruum/app`: the Ruum SDK for Node apps.
 *
 * - {@link verifyUserToken} and {@link UserTokenVerifier}: the check of the `Ruum-User` token.
 * - {@link ruumUser} (Express) and {@link withRuumUser} (`node:http`): put the verified user on the request.
 * - {@link readWorkloadToken} and {@link runtimeFetch}: the app's own token for `ruum.internal`.
 *
 * Dev mode is in `@ruum/app/dev` and the `ruum-dev-token` CLI.
 */

export {
  CLOCK_SKEW_SECONDS,
  KNOWN_ACTORS,
  MAX_LIFETIME_SECONDS,
  MAX_TOKEN_LENGTH,
  RuumConfigError,
  RuumTokenError,
  UserTokenVerifier,
  configFromEnv,
  verifyUserToken,
  type RefusalReason,
  type RuumUser,
  type VerifierConfig,
} from "./verify.js";
export { KeySet, keysFromJwks, type Jwks, type KeySetOptions } from "./keys.js";
export { ruumUser, withRuumUser, type MiddlewareOptions, type RequestWithUser } from "./middleware.js";
export {
  DEFAULT_TOKEN_FILE,
  WorkloadTokenError,
  isRuumInternal,
  readWorkloadToken,
  runtimeFetch,
  workloadAuthHeaders,
} from "./workload.js";
