//! The tool catalog: one MCP tool per entry of `contracts/mcp/tools.yaml`.
//!
//! `src/tools.json` is generated from `contracts/mcp/tools.yaml` and `contracts/openapi.yaml` by
//! `scripts/gen-tools.mjs` (so the crate needs no YAML library). It holds, per tool, the REST
//! operation (method and path template), where each input property goes (path, query or
//! header), how the body is sent, and the input JSON Schema with every `$ref` inlined. A test
//! fails when the contract changed and the file was not generated again.

use rmcp::model::{JsonObject, Tool, ToolAnnotations};
use serde::Deserialize;
use std::sync::Arc;

/// The generated catalog, as text.
pub const TOOLS_JSON: &str = include_str!("tools.json");

/// Where an input property goes in the REST request.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ParamIn {
    Path,
    Query,
    Header,
}

/// One path, query or header parameter of the operation.
#[derive(Clone, Debug, Deserialize)]
pub struct Param {
    /// The name in the contract (`node_id`, `If-Match`).
    pub name: String,
    #[serde(rename = "in")]
    pub location: ParamIn,
    /// The name of the input property (`node_id`, `if_match`).
    pub prop: String,
}

/// How the tool sends the request body.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum BodyKind {
    /// The input property `body`, as JSON.
    Json,
    /// The input properties `content` and `encoding`, as raw bytes (`write_file`).
    Content,
}

/// One tool and the REST operation it maps to.
#[derive(Clone, Debug, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    /// The `operationId`.
    pub operation: String,
    /// The operation is `x-ruum-status: planned` (specified, not built): the server does not
    /// offer the tool (decision 0004-22).
    #[serde(default)]
    pub planned: bool,
    pub description: String,
    pub notes: Option<String>,
    /// Upper case: `GET`, `POST`, ...
    pub method: String,
    /// The path template, for example `/v1/nodes/{node_id}`.
    pub path: String,
    pub params: Vec<Param>,
    pub body: Option<BodyKind>,
    pub input_schema: JsonObject,
}

impl ToolSpec {
    /// The MCP tool, with the description (and notes) for the agent and behaviour hints.
    pub fn tool(&self) -> Tool {
        let description = match &self.notes {
            Some(n) => format!("{} {}", self.description, n),
            None => self.description.clone(),
        };
        let read_only = self.method == "GET";
        let destructive = matches!(
            self.name.as_str(),
            "purge_node" | "empty_trash" | "delete_tag" | "remove_grant" | "revoke_share_link"
        ) || self.method == "DELETE";
        let annotations = ToolAnnotations::new()
            .read_only(read_only)
            .destructive(!read_only && destructive)
            .idempotent(matches!(self.method.as_str(), "GET" | "PUT" | "DELETE"))
            .open_world(false);
        Tool::new(
            self.name.clone(),
            description,
            Arc::new(self.input_schema.clone()),
        )
        .with_annotations(annotations)
    }
}

/// The generated file.
#[derive(Debug, Deserialize)]
struct ToolsFile {
    openapi_sha256: String,
    tools_sha256: String,
    tools: Vec<ToolSpec>,
}

/// All tools of the MCP server.
#[derive(Debug)]
pub struct Catalog {
    pub tools: Vec<ToolSpec>,
    /// SHA-256 of `contracts/openapi.yaml` when `tools.json` was generated.
    pub openapi_sha256: String,
    /// SHA-256 of `contracts/mcp/tools.yaml` when `tools.json` was generated.
    pub tools_sha256: String,
}

impl Catalog {
    /// Reads the generated catalog. It is part of the binary, so a failure is a build bug.
    pub fn load() -> Self {
        let file: ToolsFile =
            serde_json::from_str(TOOLS_JSON).expect("src/tools.json is valid (generated)");
        Self {
            tools: file.tools,
            openapi_sha256: file.openapi_sha256,
            tools_sha256: file.tools_sha256,
        }
    }

    /// The tool of this name, if the server offers it (planned tools are left out).
    pub fn get(&self, name: &str) -> Option<&ToolSpec> {
        self.offered().find(|t| t.name == name)
    }

    /// The tools the server offers: every tool whose operation is built (not planned).
    pub fn offered(&self) -> impl Iterator<Item = &ToolSpec> {
        self.tools.iter().filter(|t| !t.planned)
    }

    /// The MCP tools, in contract order, without the planned ones.
    pub fn mcp_tools(&self) -> Vec<Tool> {
        self.offered().map(ToolSpec::tool).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::Catalog;

    /// A tool of a planned operation is in the catalog file but never offered or callable.
    #[test]
    fn planned_tools_are_not_offered() {
        let c = Catalog::load();
        let planned: Vec<&str> = c
            .tools
            .iter()
            .filter(|t| t.planned)
            .map(|t| t.name.as_str())
            .collect();
        let offered: Vec<String> = c.mcp_tools().iter().map(|t| t.name.to_string()).collect();
        for name in planned {
            assert!(!offered.iter().any(|o| o == name), "{name} is offered");
            assert!(c.get(name).is_none(), "{name} can be called");
        }
        assert_eq!(offered.len(), c.offered().count());
    }
}
