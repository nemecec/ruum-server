#!/usr/bin/env node
/**
 * `ruum-dev-token`: dev mode on the command line.
 *
 *   ruum-dev-token init [--dir .ruum-dev]
 *   ruum-dev-token mint --aud app:<app-id>[/preview/<deployment-id>] [--env live|preview]
 *                       [--roles a,b] [--sub pairwise:dev-user] [--tenant dev] [--actor user]
 *                       [--ttl 60] [--issuer http://localhost] [--dir .ruum-dev]
 *
 * `init` makes a local key and prints the environment the app needs; `mint` prints one token.
 */
/** Runs the CLI; returns the exit status. */
export declare function main(argv: string[], out: (s: string) => void, err: (s: string) => void): number;
