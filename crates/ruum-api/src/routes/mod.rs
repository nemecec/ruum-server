//! One router per area. The handler names are the `operationId`s of `contracts/openapi.yaml`
//! in snake_case.

pub mod agents;
pub mod audit;
pub mod auth;
pub mod blobs;
pub mod changes;
pub mod nodes;
pub mod search;
pub mod sharing;
pub mod system;
pub mod tags;
pub mod tenants;
pub mod versions;

#[cfg(test)]
mod tests;
