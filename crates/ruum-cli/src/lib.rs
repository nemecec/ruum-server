//! `ruum`: the command line client of Ruum (`docs/DESIGN.md`, section 11). It talks to the REST
//! API with an agent API key (`ruum_agent_...`), which it exchanges for short-lived access
//! tokens. Paths are from the tenant root: `/Projects/Garden/plan.md`.
//!
//! Every command prints text for people, or the API's JSON with `--json` (for scripts and
//! agents). An API error prints its Ruum code, message and hint, and the exit code is 1.
//!
//! Owned by the MCP server + CLI area.

pub mod client;
mod upload;

use anyhow::{Context as _, Result, anyhow, bail};
use clap::{Parser, Subcommand, ValueEnum};
use client::{ApiError, Body, Client, Credentials, default_credentials_path, encode_segment};
use reqwest::Method;
use serde_json::{Value, json};
use std::io::Write;
use std::path::PathBuf;

/// The default API origin (the laptop stack).
pub const DEFAULT_URL: &str = "https://ruum.localhost";

#[derive(Parser, Debug)]
#[command(name = "ruum", version, about = "The Ruum command line client")]
pub struct Cli {
    /// The base URL of the Ruum API. Default: the URL of `ruum login`, else https://ruum.localhost.
    /// A URL other than the saved one needs --api-key. http:// only for loopback and *.localhost.
    #[arg(long, env = "RUUM_URL", global = true)]
    pub url: Option<String>,
    /// An agent API key (`ruum_agent_...`). Overrides the key of `ruum login`; not saved.
    #[arg(long, env = "RUUM_API_KEY", hide_env_values = true, global = true)]
    pub api_key: Option<String>,
    /// The credentials file. Default: ~/.config/ruum/credentials.json.
    #[arg(long, env = "RUUM_CREDENTIALS", global = true)]
    pub credentials: Option<PathBuf>,
    /// Print the API's JSON instead of text.
    #[arg(long, global = true)]
    pub json: bool,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum RoleArg {
    View,
    Comment,
    Edit,
    Manage,
}

impl RoleArg {
    fn as_str(self) -> &'static str {
        match self {
            RoleArg::View => "view",
            RoleArg::Comment => "comment",
            RoleArg::Edit => "edit",
            RoleArg::Manage => "manage",
        }
    }
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Sign in with an agent API key: exchange it for a token and save both.
    Login {
        /// The API key. Default: --api-key, RUUM_API_KEY, or one line from stdin.
        #[arg(long)]
        key: Option<String>,
    },
    /// Forget the saved key and token.
    Logout,
    /// Show who you are: the agent, its user and its scope.
    Whoami,
    /// List a folder (or show one file).
    Ls {
        /// A path from the root. Default: the root.
        path: Option<String>,
    },
    /// Make a folder.
    Mkdir {
        path: String,
        /// Make missing parent folders too, and do not fail if the folder exists.
        #[arg(short, long)]
        parents: bool,
    },
    /// Upload a local file: a new file, or a new version of an existing file.
    Upload {
        /// The local file.
        local: PathBuf,
        /// The remote file or folder. Default: the root, with the local file name.
        remote: Option<String>,
        /// When the name is taken in the folder, add " (2)" instead of failing.
        #[arg(long)]
        rename: bool,
    },
    /// Download a file.
    Download {
        remote: String,
        /// The local file, or `-` for stdout. Default: the remote name in this folder.
        local: Option<String>,
        /// A version ID (default: the head version).
        #[arg(long)]
        version: Option<String>,
    },
    /// Move or rename a file or folder.
    Mv { from: String, to: String },
    /// Move a file or folder to the trash.
    Rm {
        path: String,
        /// Delete it for ever after trashing (cannot be undone).
        #[arg(long)]
        purge: bool,
    },
    /// List the versions of a file, newest first.
    Versions { path: String },
    /// Share a file or folder with a person, or make a share link.
    Share {
        path: String,
        /// The person's email.
        #[arg(long, conflicts_with_all = ["user", "link"])]
        email: Option<String>,
        /// The person's user ID.
        #[arg(long, conflicts_with = "link")]
        user: Option<String>,
        /// Make a share link instead of a grant.
        #[arg(long)]
        link: bool,
        #[arg(long, value_enum, default_value = "view")]
        role: RoleArg,
        /// For a link: when it ends (RFC 3339).
        #[arg(long, requires = "link")]
        expires: Option<String>,
    },
    /// Show the change journal.
    Changes {
        /// Start after this cursor (the `next cursor` of an earlier call).
        #[arg(long)]
        cursor: Option<String>,
        #[arg(long, default_value_t = 100)]
        limit: u32,
        /// Keep waiting for new changes (long poll).
        #[arg(long)]
        follow: bool,
    },
    /// List your agents (an agent sees itself).
    Agents,
}

/// Runs the command line `args` (without the program name), writing to `out`.
pub async fn run_args<I, S>(args: I, out: &mut dyn Write) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: Into<std::ffi::OsString> + Clone,
{
    let cli = Cli::try_parse_from(
        std::iter::once("ruum".into()).chain(args.into_iter().map(Into::into)),
    )?;
    run(cli, out).await
}

/// Runs one parsed command.
pub async fn run(cli: Cli, out: &mut dyn Write) -> Result<()> {
    let creds_path = cli
        .credentials
        .clone()
        .unwrap_or_else(default_credentials_path);
    match &cli.command {
        Command::Login { key } => return login(&cli, key.clone(), creds_path, out).await,
        Command::Logout => {
            match std::fs::remove_file(&creds_path) {
                Ok(()) => writeln!(out, "signed out ({} removed)", creds_path.display())?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    writeln!(out, "not signed in")?
                }
                Err(e) => return Err(e).context("remove the credentials file"),
            }
            return Ok(());
        }
        _ => {}
    }
    let mut c = client_for(&cli, creds_path)?;
    let mut ctx = Ctx {
        c: &mut c,
        out,
        json: cli.json,
    };
    match cli.command {
        Command::Login { .. } | Command::Logout => unreachable!("handled above"),
        Command::Whoami => ctx.whoami().await,
        Command::Ls { path } => ctx.ls(path.as_deref().unwrap_or("")).await,
        Command::Mkdir { path, parents } => ctx.mkdir(&path, parents).await,
        Command::Upload {
            local,
            remote,
            rename,
        } => ctx.upload(&local, remote.as_deref(), rename).await,
        Command::Download {
            remote,
            local,
            version,
        } => ctx.download(&remote, local.as_deref(), version).await,
        Command::Mv { from, to } => ctx.mv(&from, &to).await,
        Command::Rm { path, purge } => ctx.rm(&path, purge).await,
        Command::Versions { path } => ctx.versions(&path).await,
        Command::Share {
            path,
            email,
            user,
            link,
            role,
            expires,
        } => ctx.share(&path, email, user, link, role, expires).await,
        Command::Changes {
            cursor,
            limit,
            follow,
        } => ctx.changes(cursor, limit, follow).await,
        Command::Agents => ctx.agents().await,
    }
}

/// Prints an error of `run` for people: the Ruum code, message and hint.
pub fn report(e: &anyhow::Error) -> String {
    if let Some(api) = e.downcast_ref::<ApiError>() {
        return format!("error: {api}");
    }
    format!("error: {e:#}")
}

fn client_for(cli: &Cli, creds_path: PathBuf) -> Result<Client> {
    if let Some(key) = &cli.api_key {
        let saved = Credentials::load(&creds_path).ok().flatten();
        let url = cli
            .url
            .clone()
            .or(saved.map(|s| s.url))
            .unwrap_or_else(|| DEFAULT_URL.into());
        return Client::new(&url, key, None);
    }
    let Some(creds) = Credentials::load(&creds_path)? else {
        bail!("not signed in\nhint: run `ruum login --key ruum_agent_...`, or set RUUM_API_KEY");
    };
    // The saved key belongs to the saved URL: it is never sent to another host, and the saved
    // URL is never changed here (only `ruum login` writes it).
    if let Some(url) = &cli.url {
        let url = url.trim().trim_end_matches('/');
        if url != creds.url.trim_end_matches('/') {
            bail!(
                "the saved key belongs to {}, not {url}\nhint: run `ruum login --url {url} --key ruum_agent_...` again, or give --api-key with --url",
                creds.url
            );
        }
    }
    Client::from_credentials(creds, creds_path)
}

async fn login(
    cli: &Cli,
    key: Option<String>,
    creds_path: PathBuf,
    out: &mut dyn Write,
) -> Result<()> {
    let key = match key.or_else(|| cli.api_key.clone()) {
        Some(k) => k,
        None => {
            let mut line = String::new();
            std::io::stdin()
                .read_line(&mut line)
                .context("read the API key from stdin")?;
            line.trim().to_string()
        }
    };
    let url = cli.url.clone().unwrap_or_else(|| DEFAULT_URL.into());
    let mut c = Client::new(&url, key.trim(), None)?;
    c.exchange().await?;
    c.persist_to(creds_path.clone())?;
    let me = c.get("/v1/me", &[]).await?;
    if cli.json {
        writeln!(out, "{}", serde_json::to_string_pretty(&me)?)?;
    } else {
        writeln!(out, "signed in to {} as {}", c.url(), describe_me(&me))?;
        writeln!(out, "credentials saved in {}", creds_path.display())?;
    }
    Ok(())
}

fn describe_me(me: &Value) -> String {
    let user = me["user"]["email"]
        .as_str()
        .or(me["user"]["display_name"].as_str())
        .unwrap_or("?");
    match me["agent"]["name"].as_str() {
        Some(agent) => format!(
            "agent \"{agent}\" ({}) for {user}",
            me["agent"]["id"].as_str().unwrap_or("?")
        ),
        None => user.to_string(),
    }
}

/// The parts of a path from the root: no empty names, no `.`.
pub fn split_path(path: &str) -> Vec<String> {
    path.split('/')
        .filter(|s| !s.is_empty() && *s != ".")
        .map(str::to_string)
        .collect()
}

fn is_not_found(e: &anyhow::Error) -> bool {
    e.downcast_ref::<ApiError>()
        .is_some_and(|a| a.code == "not_found")
}

fn s(v: &Value, key: &str) -> String {
    match &v[key] {
        Value::String(s) => s.clone(),
        Value::Null => "-".into(),
        other => other.to_string(),
    }
}

/// Bytes for people: 1.2 MB.
fn human_size(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "kB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1000.0 && i < UNITS.len() - 1 {
        v /= 1000.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

struct Ctx<'a> {
    c: &'a mut Client,
    out: &'a mut dyn Write,
    json: bool,
}

impl Ctx<'_> {
    fn print_json(&mut self, v: &Value) -> Result<()> {
        writeln!(self.out, "{}", serde_json::to_string_pretty(v)?)?;
        Ok(())
    }

    /// The node at `path` (the root for an empty path).
    async fn resolve(&mut self, path: &str) -> Result<Value> {
        let parts = split_path(path);
        if parts.is_empty() {
            return self.c.get("/v1/nodes/root", &[]).await;
        }
        let encoded: Vec<String> = parts.iter().map(|p| encode_segment(p)).collect();
        self.c
            .get(&format!("/v1/nodes/by-path/{}", encoded.join("/")), &[])
            .await
            .map_err(|e| {
                if is_not_found(&e) {
                    anyhow!("{path}: no such file or folder")
                } else {
                    e
                }
            })
    }

    /// The node at `path`, or `None` when it does not exist.
    async fn try_resolve(&mut self, path: &str) -> Result<Option<Value>> {
        let parts = split_path(path);
        if parts.is_empty() {
            return self.c.get("/v1/nodes/root", &[]).await.map(Some);
        }
        let encoded: Vec<String> = parts.iter().map(|p| encode_segment(p)).collect();
        match self
            .c
            .get(&format!("/v1/nodes/by-path/{}", encoded.join("/")), &[])
            .await
        {
            Ok(v) => Ok(Some(v)),
            Err(e) if is_not_found(&e) => Ok(None),
            Err(e) => Err(e),
        }
    }

    async fn whoami(&mut self) -> Result<()> {
        let me = self.c.get("/v1/me", &[]).await?;
        if self.json {
            return self.print_json(&me);
        }
        writeln!(self.out, "{}", describe_me(&me))?;
        if let Some(t) = me["bound_tenant_id"].as_str() {
            writeln!(self.out, "tenant: {t}")?;
        }
        let scope = &me["agent"]["scope"];
        if scope.is_object() {
            writeln!(
                self.out,
                "scope: subtree {}, max role {}, expires {}",
                s(scope, "subtree"),
                s(scope, "max_role"),
                s(scope, "expires_at")
            )?;
        }
        Ok(())
    }

    async fn ls(&mut self, path: &str) -> Result<()> {
        let node = self.resolve(path).await?;
        if node["kind"] != "folder" {
            if self.json {
                return self.print_json(&node);
            }
            return self.print_nodes(&[node]);
        }
        let id = s(&node, "id");
        let items = self
            .c
            .all_pages(
                &format!("/v1/nodes/{id}/children"),
                &[("limit", "1000".into())],
            )
            .await?;
        if self.json {
            return self.print_json(&Value::Array(items));
        }
        self.print_nodes(&items)
    }

    fn print_nodes(&mut self, items: &[Value]) -> Result<()> {
        for n in items {
            let kind = n["kind"].as_str().unwrap_or("?");
            let size = if kind == "folder" {
                "-".to_string()
            } else {
                human_size(n["size_bytes"].as_u64().unwrap_or(0))
            };
            let suffix = if kind == "folder" { "/" } else { "" };
            writeln!(
                self.out,
                "{:<6} {:>9}  {:<20}  {}{}",
                kind,
                size,
                n["updated_at"].as_str().unwrap_or("-"),
                n["name"].as_str().unwrap_or("?"),
                suffix
            )?;
        }
        Ok(())
    }

    async fn create_folder(&mut self, parent: &str, name: &str) -> Result<Value> {
        let w = self
            .c
            .post(
                "/v1/nodes",
                json!({"parent_id": parent, "name": name, "kind": "folder"}),
            )
            .await?;
        Ok(w["node"].clone())
    }

    async fn mkdir(&mut self, path: &str, parents: bool) -> Result<()> {
        let parts = split_path(path);
        let Some((last, dirs)) = parts.split_last() else {
            bail!("mkdir: give a folder path, for example /Projects/Garden");
        };
        let mut parent = self.resolve("").await?;
        let mut walked = String::new();
        for d in dirs {
            walked = format!("{walked}/{d}");
            parent = match self.try_resolve(&walked).await? {
                Some(n) if n["kind"] == "folder" => n,
                Some(_) => bail!("{walked} is not a folder"),
                None if parents => self.create_folder(&s(&parent, "id"), d).await?,
                None => bail!("{walked}: no such folder\nhint: use mkdir -p to make parents"),
            };
        }
        walked = format!("{walked}/{last}");
        let node = match self.try_resolve(&walked).await? {
            Some(n) if parents && n["kind"] == "folder" => n,
            Some(_) => bail!("{walked} already exists"),
            None => self.create_folder(&s(&parent, "id"), last).await?,
        };
        if self.json {
            return self.print_json(&node);
        }
        writeln!(self.out, "created {walked}/ ({})", s(&node, "id"))?;
        Ok(())
    }

    async fn upload(
        &mut self,
        local: &std::path::Path,
        remote: Option<&str>,
        rename: bool,
    ) -> Result<()> {
        let local_name = local
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| anyhow!("{}: not a file name", local.display()))?
            .to_string();
        let remote = remote.unwrap_or("/");
        let on_conflict = if rename { "rename" } else { "fail" };
        let target = match self.try_resolve(remote).await? {
            Some(n) if n["kind"] == "folder" => json!({
                "target": "new_file", "parent_id": s(&n, "id"), "name": local_name,
                "on_conflict": on_conflict,
            }),
            Some(n) if n["kind"] == "file" => json!({
                "target": "new_version", "node_id": s(&n, "id"),
                "base_version": n["head_version"],
            }),
            Some(_) => bail!("{remote}: cannot upload onto a link or typed node"),
            None => {
                let mut parts = split_path(remote);
                let name = parts.pop().unwrap_or(local_name);
                let parent = self.resolve(&parts.join("/")).await?;
                if parent["kind"] != "folder" {
                    bail!("/{}: not a folder", parts.join("/"));
                }
                json!({
                    "target": "new_file", "parent_id": s(&parent, "id"), "name": name,
                    "on_conflict": on_conflict,
                })
            }
        };
        let result = upload::upload_file(self.c, local, target).await?;
        if self.json {
            return self.print_json(&result.write);
        }
        writeln!(
            self.out,
            "uploaded {} as {} (version {}, {}, {} of {} chunks sent)",
            local.display(),
            s(&result.write["node"], "name"),
            s(&result.write["version"], "id"),
            human_size(result.size),
            result.sent,
            result.chunks
        )?;
        Ok(())
    }

    async fn download(
        &mut self,
        remote: &str,
        local: Option<&str>,
        version: Option<String>,
    ) -> Result<()> {
        let node = self.resolve(remote).await?;
        if node["kind"] != "file" {
            bail!("{remote}: not a file");
        }
        let id = s(&node, "id");
        let mut query = Vec::new();
        if let Some(v) = version {
            query.push(("version", v));
        }
        let mut res = self
            .c
            .send(
                Method::GET,
                &format!("/v1/nodes/{id}/content"),
                &query,
                Body::None,
                &[],
            )
            .await?;
        let name = s(&node, "name");
        let target = local.unwrap_or(&name);
        let mut total = 0u64;
        if target == "-" {
            while let Some(chunk) = res.chunk().await? {
                self.out.write_all(&chunk)?;
                total += chunk.len() as u64;
            }
            let _ = total;
            return Ok(());
        }
        let mut path = PathBuf::from(target);
        if path.is_dir() {
            path = path.join(&name);
        }
        let tmp = path.with_extension("ruum-part");
        {
            let mut f =
                std::fs::File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?;
            while let Some(chunk) = res.chunk().await? {
                f.write_all(&chunk)?;
                total += chunk.len() as u64;
            }
            f.flush()?;
        }
        std::fs::rename(&tmp, &path).with_context(|| format!("write {}", path.display()))?;
        if self.json {
            return self.print_json(&json!({"path": path, "size": total, "node": node}));
        }
        writeln!(
            self.out,
            "downloaded {remote} to {} ({})",
            path.display(),
            human_size(total)
        )?;
        Ok(())
    }

    async fn mv(&mut self, from: &str, to: &str) -> Result<()> {
        let src = self.resolve(from).await?;
        if src["parent_id"].is_null() {
            bail!("the root folder cannot be moved");
        }
        let id = s(&src, "id");
        let w = match self.try_resolve(to).await? {
            Some(dst) if dst["kind"] == "folder" => {
                self.c
                    .post(
                        &format!("/v1/nodes/{id}/move"),
                        json!({"parent_id": dst["id"]}),
                    )
                    .await?
            }
            Some(_) => bail!("{to} already exists"),
            None => {
                let mut parts = split_path(to);
                let Some(name) = parts.pop() else {
                    bail!("mv: give a target path");
                };
                let parent = self.resolve(&parts.join("/")).await?;
                if parent["kind"] != "folder" {
                    bail!("/{}: not a folder", parts.join("/"));
                }
                if parent["id"] == src["parent_id"] {
                    self.c
                        .post(&format!("/v1/nodes/{id}/rename"), json!({"name": name}))
                        .await?
                } else {
                    self.c
                        .post(
                            &format!("/v1/nodes/{id}/move"),
                            json!({"parent_id": parent["id"], "name": name}),
                        )
                        .await?
                }
            }
        };
        if self.json {
            return self.print_json(&w);
        }
        writeln!(self.out, "moved {from} to {to}")?;
        Ok(())
    }

    async fn rm(&mut self, path: &str, purge: bool) -> Result<()> {
        let node = self.resolve(path).await?;
        if node["parent_id"].is_null() {
            bail!("the root folder cannot be removed");
        }
        let id = s(&node, "id");
        let w = self
            .c
            .json(
                Method::POST,
                &format!("/v1/nodes/{id}/trash"),
                &[],
                Body::None,
            )
            .await?;
        if purge {
            let p = self
                .c
                .json(Method::DELETE, &format!("/v1/nodes/{id}"), &[], Body::None)
                .await?;
            if self.json {
                return self.print_json(&p);
            }
            writeln!(self.out, "deleted {path} for ever")?;
            return Ok(());
        }
        if self.json {
            return self.print_json(&w);
        }
        writeln!(self.out, "moved {path} to the trash")?;
        Ok(())
    }

    async fn versions(&mut self, path: &str) -> Result<()> {
        let node = self.resolve(path).await?;
        let id = s(&node, "id");
        let items = self
            .c
            .all_pages(
                &format!("/v1/nodes/{id}/versions"),
                &[("limit", "1000".into())],
            )
            .await?;
        if self.json {
            return self.print_json(&Value::Array(items));
        }
        let head = node["head_version"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        for v in &items {
            let vid = s(v, "id");
            let actor = format!(
                "{}:{}",
                v["actor"]["kind"].as_str().unwrap_or("?"),
                v["actor"]["id"].as_str().unwrap_or("?")
            );
            writeln!(
                self.out,
                "{} {}  {}  {:>9}  {}{}",
                if vid == head { "*" } else { " " },
                vid,
                v["created_at"].as_str().unwrap_or("-"),
                human_size(v["size_bytes"].as_u64().unwrap_or(0)),
                actor,
                v["label"]
                    .as_str()
                    .map(|l| format!("  \"{l}\""))
                    .unwrap_or_default()
            )?;
        }
        Ok(())
    }

    async fn share(
        &mut self,
        path: &str,
        email: Option<String>,
        user: Option<String>,
        link: bool,
        role: RoleArg,
        expires: Option<String>,
    ) -> Result<()> {
        let node = self.resolve(path).await?;
        let id = s(&node, "id");
        if link {
            if matches!(role, RoleArg::Manage) {
                bail!("a share link can give view, comment or edit");
            }
            let mut body = json!({"role": role.as_str()});
            if let Some(e) = expires {
                body["expires_at"] = Value::String(e);
            }
            let w = self
                .c
                .post(&format!("/v1/nodes/{id}/share-links"), body)
                .await?;
            if self.json {
                return self.print_json(&w);
            }
            let l = &w["share_link"];
            writeln!(
                self.out,
                "share link ({}) for {path}: {}",
                s(l, "role"),
                l["url"]
                    .as_str()
                    .or(l["token"].as_str())
                    .unwrap_or("(no URL in the answer)")
            )?;
            return Ok(());
        }
        let grantee = match (email, user) {
            (Some(e), None) => json!({"type": "email", "email": e}),
            (None, Some(u)) => json!({"type": "user", "id": u}),
            _ => bail!("share: give --email, --user or --link"),
        };
        let w = self
            .c
            .post(
                &format!("/v1/nodes/{id}/grants"),
                json!({"grantee": grantee, "role": role.as_str()}),
            )
            .await?;
        if self.json {
            return self.print_json(&w);
        }
        writeln!(self.out, "shared {path} ({})", role.as_str())?;
        Ok(())
    }

    async fn changes(&mut self, cursor: Option<String>, limit: u32, follow: bool) -> Result<()> {
        let mut cursor = cursor;
        loop {
            let mut q = vec![("limit", limit.to_string())];
            if let Some(c) = &cursor {
                q.push(("cursor", c.clone()));
            }
            if follow {
                q.push(("wait", "30".into()));
            }
            let page = self.c.get("/v1/changes", &q).await?;
            if self.json {
                self.print_json(&page)?;
            } else {
                for e in page["items"].as_array().into_iter().flatten() {
                    writeln!(
                        self.out,
                        "{:>6}  {}  {:<20} {}  {}:{}",
                        s(e, "seq"),
                        e["at"].as_str().unwrap_or("-"),
                        e["type"].as_str().unwrap_or("?"),
                        s(e, "node_id"),
                        e["actor"]["kind"].as_str().unwrap_or("?"),
                        e["actor"]["id"].as_str().unwrap_or("?"),
                    )?;
                }
                for r in page["resync"].as_array().into_iter().flatten() {
                    writeln!(self.out, "resync: {r}")?;
                }
            }
            cursor = page["next_cursor"].as_str().map(str::to_string);
            let more = page["has_more"].as_bool().unwrap_or(false);
            if !follow && !more {
                if !self.json {
                    writeln!(
                        self.out,
                        "next cursor: {}",
                        cursor.as_deref().unwrap_or("-")
                    )?;
                }
                return Ok(());
            }
            self.out.flush()?;
        }
    }

    async fn agents(&mut self) -> Result<()> {
        let items = self
            .c
            .all_pages("/v1/agents", &[("limit", "1000".into())])
            .await?;
        if self.json {
            return self.print_json(&Value::Array(items));
        }
        for a in &items {
            let state = if a["disabled_at"].is_null() {
                "active"
            } else {
                "disabled"
            };
            writeln!(
                self.out,
                "{}  {:<8}  {}  (max role {}, subtree {})",
                s(a, "id"),
                state,
                s(a, "name"),
                s(&a["scope"], "max_role"),
                s(&a["scope"], "subtree"),
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_split_from_the_root() {
        assert!(split_path("/").is_empty());
        assert!(split_path("").is_empty());
        assert_eq!(split_path("/a//b/./c/"), vec!["a", "b", "c"]);
    }

    #[test]
    fn sizes_for_people() {
        assert_eq!(human_size(12), "12 B");
        assert_eq!(human_size(1_500), "1.5 kB");
        assert_eq!(human_size(2_000_000), "2.0 MB");
    }

    #[test]
    fn the_command_line_parses() {
        Cli::try_parse_from(["ruum", "share", "/a", "--email", "x@y.z", "--role", "edit"]).unwrap();
        Cli::try_parse_from([
            "ruum",
            "share",
            "/a",
            "--link",
            "--expires",
            "2030-01-01T00:00:00Z",
        ])
        .unwrap();
        assert!(Cli::try_parse_from(["ruum", "share", "/a", "--email", "e", "--link"]).is_err());
        Cli::try_parse_from(["ruum", "--json", "ls"]).unwrap();
        Cli::try_parse_from(["ruum", "mkdir", "-p", "/a/b"]).unwrap();
    }
}
