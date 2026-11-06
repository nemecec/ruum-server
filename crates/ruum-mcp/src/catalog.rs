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

    pub fn get(&self, name: &str) -> Option<&ToolSpec> {
        self.tools.iter().find(|t| t.name == name)
    }

    /// The MCP tools, in contract order.
    pub fn mcp_tools(&self) -> Vec<Tool> {
        self.tools.iter().map(ToolSpec::tool).collect()
    }
}
