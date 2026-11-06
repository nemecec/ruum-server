//! The `rmcp` server handler: lists the tools of the catalog and runs tool calls.

use crate::catalog::Catalog;
use crate::dispatch::{self, Caller};
use axum::Router;
use axum::http::StatusCode;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, Implementation, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler};
use ruum_core::{ApiError, ErrorCode};
use std::sync::Arc;

const INSTRUCTIONS: &str = "Ruum is a file tree with versions, tags and sharing. \
Start with get_me (who you are and your scope) and get_root or get_node_by_path. \
Every write makes a version or a journal entry, so it can be undone by your user. \
Errors carry a code and a hint for the next step.";

/// The MCP server of one Ruum instance.
#[derive(Clone)]
pub struct RuumMcp {
    catalog: Arc<Catalog>,
    tools: Arc<Vec<Tool>>,
    api: Router,
}

impl RuumMcp {
    pub fn new(catalog: Arc<Catalog>, api: Router) -> Self {
        let tools = Arc::new(catalog.mcp_tools());
        Self {
            catalog,
            tools,
            api,
        }
    }
}

impl ServerHandler for RuumMcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("ruum", env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(self.tools.as_ref().clone()))
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.tools.iter().find(|t| t.name == name).cloned()
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let Some(spec) = self.catalog.get(&request.name) else {
            return Err(ErrorData::invalid_params(
                format!("unknown tool {}", request.name),
                None,
            ));
        };
        let caller = context
            .extensions
            .get::<axum::http::request::Parts>()
            .and_then(|p| p.extensions.get::<Caller>())
            .cloned();
        let Some(caller) = caller else {
            // The auth layer always sets the caller; this is a wiring bug.
            return Ok(dispatch::error_result(
                &ApiError::new(ErrorCode::Unauthenticated, "no caller on the request")
                    .with_hint("send an access token in the Authorization header"),
                StatusCode::UNAUTHORIZED,
            )
            .into());
        };
        let args = request.arguments.unwrap_or_default();
        Ok(dispatch::call(&self.api, &caller, spec, args).await.into())
    }
}
