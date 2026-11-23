use clap::{Parser, Subcommand};
use cold_vtl_core::{Barcode, VtlId};
use reqwest::{Client, Method, header};
use serde_json::Value;
use std::{
    error::Error,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Parser)]
#[command(name = "cold-vtl", about = "VaultCenter operator CLI")]
struct Cli {
    /// Library context for every media, storage, maintenance and job command.
    #[arg(long, global = true, env = "COLD_VTL_ID")]
    vtl: Option<VtlId>,
    #[arg(
        long,
        env = "COLD_VTL_API_URL",
        default_value = "https://127.0.0.1:8443"
    )]
    api: String,
    /// PEM CA certificate for an internal or lab certificate authority.
    #[arg(long, env = "COLD_VTL_CA_CERT")]
    ca_cert: Option<PathBuf>,
    /// Read an API token from a protected file instead of saved login credentials.
    #[arg(long, env = "COLD_VTL_TOKEN_FILE")]
    token_file: Option<PathBuf>,
    #[arg(long, env = "COLD_VTL_CREDENTIALS")]
    credentials: Option<PathBuf>,
    /// Only for isolated development environments.
    #[arg(long)]
    allow_insecure_http: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Discover configured libraries.
    Vtl {
        #[command(subcommand)]
        command: VtlCommand,
    },
    /// Read-only cloud status, bucket scope and preliminary archive checks.
    Storage,
    /// Managed bucket recall: confirm and observe the request.
    Recall {
        #[command(subcommand)]
        command: RecallCommand,
    },
    Archive {
        #[command(subcommand)]
        command: ArchiveCommand,
    },
    Login {
        #[arg(long)]
        username: String,
        #[arg(long)]
        password_stdin: bool,
    },
    Logout,
    Status,
    Maintenance {
        #[command(subcommand)]
        command: MaintenanceCommand,
    },
    Media {
        #[command(subcommand)]
        command: MediaCommand,
    },
    Jobs {
        #[command(subcommand)]
        command: JobsCommand,
    },
}

#[derive(Subcommand)]
enum VtlCommand {
    List,
    Show {
        id: VtlId,
    },
    /// Inspect host tape storage for this library, including its permanent lock.
    TapeStorage {
        id: VtlId,
    },
    /// Inspect isolated hardware provisioning and activation.
    Provisioning {
        id: VtlId,
    },
}

#[derive(Subcommand)]
enum MaintenanceCommand {
    Status,
    Enter,
    Leave,
    Events,
}

#[derive(Subcommand)]
enum MediaCommand {
    /// List reintegration journals and interrupted operations.
    Reintegrations,
    /// Install a completed restore and resume PBS; preserve the previous cartridge.
    Reintegrate {
        barcode: String,
        #[arg(long)]
        restore_job: String,
    },
    /// Recover or undo a reintegration while preserving the replaced media.
    Rollback {
        operation_id: String,
    },
    List,
    Seal {
        barcode: String,
    },
    Export {
        barcode: String,
    },
    Generations {
        barcode: String,
    },
    Reconcile {
        barcode: String,
    },
    Restore {
        barcode: String,
        #[arg(long, value_parser=clap::value_parser!(u64).range(1..))]
        generation: u64,
        /// New folder name within the appliance restore directory.
        #[arg(long)]
        destination: Option<String>,
    },
}

#[derive(Subcommand)]
enum JobsCommand {
    List,
    Watch {
        id: String,
        #[arg(long, default_value_t = 2, value_parser=clap::value_parser!(u64).range(1..=3600))]
        interval_seconds: u64,
    },
}

#[derive(Subcommand)]
enum RecallCommand {
    Status,
    Refresh,
    /// Check the archived bucket and request its recall in one step.
    Submit {
        #[arg(long)]
        confirm_bucket: String,
        #[arg(long)]
        acknowledge_retrieval_cost: bool,
    },
}

#[derive(Subcommand)]
enum ArchiveCommand {
    Status,
    Refresh,
    /// Verify the bucket and request its archiving in one step.
    Submit {
        #[arg(long)]
        confirm_bucket: String,
        #[arg(long)]
        acknowledge_billing: bool,
        /// Required only for a bucket VaultCenter did not create.
        #[arg(long)]
        exclusive_bucket: bool,
        #[arg(long)]
        next_bucket: Option<String>,
    },
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let cli = Cli::parse();
    let api_url = reqwest::Url::parse(&cli.api)?;
    if api_url.host_str().is_none()
        || !api_url.username().is_empty()
        || api_url.password().is_some()
        || api_url.path() != "/"
        || api_url.query().is_some()
        || api_url.fragment().is_some()
    {
        return Err(
            "API URL must be an origin without credentials, path, query or fragment".into(),
        );
    }
    if api_url.scheme() != "https" && !(api_url.scheme() == "http" && cli.allow_insecure_http) {
        return Err("HTTPS required".into());
    }
    let api_owned = api_url.origin().ascii_serialization();
    let api = api_owned.as_str();
    let using_token_file = cli.token_file.is_some();
    let credentials = cli.credentials.unwrap_or_else(default_credentials);
    let mut builder = Client::builder()
        .timeout(Duration::from_secs(
            if matches!(
                &cli.command,
                Command::Storage
                    | Command::Maintenance {
                        command: MaintenanceCommand::Enter | MaintenanceCommand::Leave
                    }
            ) {
                310
            } else {
                30
            },
        ))
        .redirect(reqwest::redirect::Policy::none());
    if let Some(path) = cli.ca_cert {
        builder = builder.add_root_certificate(reqwest::Certificate::from_pem(&fs::read(path)?)?);
    }
    if !matches!(&cli.command, Command::Login { .. }) {
        let token = if let Some(path) = cli.token_file {
            Some(read_protected(&path)?.trim().to_owned())
        } else if credentials.exists() {
            let saved: Value = serde_json::from_str(&read_protected(&credentials)?)?;
            if saved.get("api").and_then(Value::as_str) != Some(api) {
                return Err("saved credentials belong to another API origin; use login or a separate --credentials file".into());
            }
            Some(
                saved
                    .get("token")
                    .and_then(Value::as_str)
                    .ok_or("invalid credentials file")?
                    .to_owned(),
            )
        } else {
            None
        };
        if let Some(token) = token {
            let mut value = header::HeaderValue::from_str(&format!("Bearer {token}"))?;
            value.set_sensitive(true);
            let mut headers = header::HeaderMap::new();
            headers.insert(header::AUTHORIZATION, value);
            builder = builder.default_headers(headers);
        }
    }
    let client = builder.build()?;

    match cli.command {
        Command::Vtl { command } => {
            let path = match command {
                VtlCommand::List => "/v1/vtls".to_owned(),
                VtlCommand::Show { id } => format!("/v1/vtls/{id}"),
                VtlCommand::TapeStorage { id } => format!("/v1/vtls/{id}/settings/tape-storage"),
                VtlCommand::Provisioning { id } => format!("/v1/vtls/{id}/provisioning"),
            };
            print(request(&client, cli.vtl, Method::GET, api, &path).await?)?;
        }
        Command::Login {
            username,
            password_stdin,
        } => {
            let password = if password_stdin {
                let mut s = String::new();
                std::io::stdin().take(1026).read_to_string(&mut s)?;
                s.trim_end_matches(['\r', '\n']).to_owned()
            } else {
                rpassword::prompt_password("Password: ")?
            };
            let response = request_body(
                &client,
                cli.vtl,
                Method::POST,
                api,
                "/auth/login/token",
                serde_json::json!({"username":username,"password":password}),
            )
            .await?;
            let token = response
                .get("token")
                .and_then(Value::as_str)
                .ok_or("server returned no token")?;
            write_credentials(
                &credentials,
                &serde_json::json!({"api":api,"token":token,"expires_at":response.get("expires_at")}),
            )?;
            println!(
                "Logged in as {username}; credentials saved to {}",
                credentials.display()
            );
        }
        Command::Logout => {
            request(&client, cli.vtl, Method::POST, api, "/auth/logout").await?;
            if !using_token_file && credentials.exists() {
                fs::remove_file(&credentials)?;
            }
            println!("Logged out; API token revoked.");
        }
        Command::Maintenance { command } => {
            let (method, path) = match command {
                MaintenanceCommand::Status => (Method::GET, "/v1/maintenance"),
                MaintenanceCommand::Events => (Method::GET, "/v1/maintenance/events"),
                MaintenanceCommand::Enter => (Method::POST, "/v1/maintenance/enter"),
                MaintenanceCommand::Leave => (Method::POST, "/v1/maintenance/leave"),
            };
            print(request(&client, cli.vtl, method, api, path).await?)?;
        }
        Command::Status => print(request(&client, cli.vtl, Method::GET, api, "/v1/status").await?)?,
        Command::Storage => {
            print(request(&client, cli.vtl, Method::GET, api, "/v1/storage").await?)?
        }
        Command::Recall { command } => {
            let (method, path, body) = match command {
                RecallCommand::Status => {
                    (Method::GET, "/v1/storage/archive", serde_json::json!({}))
                }
                RecallCommand::Refresh => (
                    Method::POST,
                    "/v1/storage/archive/refresh",
                    serde_json::json!({}),
                ),
                RecallCommand::Submit {
                    confirm_bucket,
                    acknowledge_retrieval_cost,
                } => (
                    Method::POST,
                    "/v1/storage/recall",
                    serde_json::json!({"confirm_bucket":confirm_bucket,"acknowledge_retrieval_cost":acknowledge_retrieval_cost}),
                ),
            };
            print(request_body(&client, cli.vtl, method, api, path, body).await?)?;
        }
        Command::Archive { command } => {
            let (method, path, body) = match command {
                ArchiveCommand::Status => {
                    (Method::GET, "/v1/storage/archive", serde_json::json!({}))
                }
                ArchiveCommand::Refresh => (
                    Method::POST,
                    "/v1/storage/archive/refresh",
                    serde_json::json!({}),
                ),
                ArchiveCommand::Submit {
                    confirm_bucket,
                    acknowledge_billing,
                    exclusive_bucket,
                    next_bucket,
                } => (
                    Method::POST,
                    "/v1/storage/archive",
                    serde_json::json!({"confirm_bucket":confirm_bucket,"acknowledge_billing":acknowledge_billing,"exclusive_bucket":exclusive_bucket,"next_bucket":next_bucket}),
                ),
            };
            print(request_body(&client, cli.vtl, method, api, path, body).await?)?;
        }
        Command::Media { command } => match command {
            MediaCommand::Reintegrations => {
                print(request(&client, cli.vtl, Method::GET, api, "/v1/reintegrations").await?)?
            }
            MediaCommand::Reintegrate {
                barcode,
                restore_job,
            } => {
                let barcode = Barcode::parse(barcode)?;
                print(
                    request_body(
                        &client,
                        cli.vtl,
                        Method::POST,
                        api,
                        &format!("/v1/cartridges/{barcode}/reintegrate"),
                        serde_json::json!({"restore_job_id":restore_job}),
                    )
                    .await?,
                )?;
            }
            MediaCommand::Rollback { operation_id } => {
                let id = uuid::Uuid::parse_str(&operation_id)?;
                print(
                    request(
                        &client,
                        cli.vtl,
                        Method::POST,
                        api,
                        &format!("/v1/reintegrations/{id}/rollback"),
                    )
                    .await?,
                )?;
            }
            MediaCommand::Generations { barcode } => {
                let barcode = Barcode::parse(barcode)?;
                print(
                    request(
                        &client,
                        cli.vtl,
                        Method::GET,
                        api,
                        &format!("/v1/cartridges/{barcode}/generations"),
                    )
                    .await?,
                )?;
            }
            MediaCommand::Reconcile { barcode } => {
                let barcode = Barcode::parse(barcode)?;
                print(
                    request(
                        &client,
                        cli.vtl,
                        Method::POST,
                        api,
                        &format!("/v1/cartridges/{barcode}/reconcile"),
                    )
                    .await?,
                )?;
            }
            MediaCommand::Restore {
                barcode,
                generation,
                destination,
            } => {
                let barcode = Barcode::parse(barcode)?;
                let body = serde_json::json!({"generation":generation,"destination":destination});
                print(
                    request_body(
                        &client,
                        cli.vtl,
                        Method::POST,
                        api,
                        &format!("/v1/cartridges/{barcode}/restore"),
                        body,
                    )
                    .await?,
                )?;
            }
            MediaCommand::List => {
                print(request(&client, cli.vtl, Method::GET, api, "/v1/cartridges").await?)?
            }
            MediaCommand::Seal { barcode } => {
                let barcode = Barcode::parse(barcode)?;
                let path = format!("/v1/cartridges/{barcode}/seal");
                print(request(&client, cli.vtl, Method::POST, api, &path).await?)?;
            }
            MediaCommand::Export { barcode } => {
                let barcode = Barcode::parse(barcode)?;
                let path = format!("/v1/cartridges/{barcode}/export");
                print(request(&client, cli.vtl, Method::POST, api, &path).await?)?;
            }
        },
        Command::Jobs { command } => match command {
            JobsCommand::List => {
                print(request(&client, cli.vtl, Method::GET, api, "/v1/jobs").await?)?
            }
            JobsCommand::Watch {
                id,
                interval_seconds,
            } => loop {
                let path = format!("/v1/jobs/{id}");
                let job = request(&client, cli.vtl, Method::GET, api, &path).await?;
                print(job.clone())?;
                match job.get("status").and_then(Value::as_str) {
                    Some("completed") => break,
                    Some("failed") => return Err(format!("job {id} failed").into()),
                    _ => {}
                }
                tokio::time::sleep(Duration::from_secs(interval_seconds)).await;
            },
        },
    }
    Ok(())
}

fn scoped_path(vtl: Option<VtlId>, path: &str) -> String {
    match vtl {
        Some(id)
            if path.starts_with("/v1/") && path != "/v1/vtls" && !path.starts_with("/v1/vtls/") =>
        {
            format!("/v1/vtls/{id}{}", &path[3..])
        }
        _ => path.to_owned(),
    }
}

async fn request(
    client: &Client,
    vtl: Option<VtlId>,
    method: Method,
    api: &str,
    path: &str,
) -> Result<Value, Box<dyn Error>> {
    request_body(client, vtl, method, api, path, serde_json::json!({})).await
}

async fn request_body(
    client: &Client,
    vtl: Option<VtlId>,
    method: Method,
    api: &str,
    path: &str,
    body: Value,
) -> Result<Value, Box<dyn Error>> {
    let path = scoped_path(vtl, path);
    let mut request = client.request(method.clone(), format!("{api}{path}"));
    if method == Method::POST {
        request = request.json(&body);
    }
    let response = request.send().await?;
    let status = response.status();
    let body: Value = response.json().await?;
    if !status.is_success() {
        let message = body
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("API request failed");
        return Err(format!("{status}: {message}").into());
    }
    Ok(body)
}

fn print(value: Value) -> Result<(), serde_json::Error> {
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

fn default_credentials() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
        });
    base.join("cold-vtl/credentials.json")
}
fn read_protected(path: &Path) -> Result<String, Box<dyn Error>> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() {
        return Err("credentials must be a regular file".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err("credentials file must have mode 0600".into());
        }
    }
    Ok(fs::read_to_string(path)?)
}
fn write_credentials(path: &Path, value: &Value) -> Result<(), Box<dyn Error>> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    file.write_all(serde_json::to_string(value)?.as_bytes())?;
    file.sync_all()?;
    fs::rename(temporary, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn archive_and_recall_are_single_step_commands() {
        assert!(Cli::try_parse_from(["cold-vtl", "archive", "prepare"]).is_err());
        assert!(Cli::try_parse_from(["cold-vtl", "recall", "prepare"]).is_err());
        assert!(
            Cli::try_parse_from([
                "cold-vtl",
                "archive",
                "submit",
                "--confirm-bucket",
                "vc-a",
                "--acknowledge-billing"
            ])
            .is_ok()
        );
        assert!(
            Cli::try_parse_from([
                "cold-vtl",
                "recall",
                "submit",
                "--confirm-bucket",
                "vc-a",
                "--acknowledge-retrieval-cost"
            ])
            .is_ok()
        );
    }
    #[test]
    fn vtl_selection_routes_all_business_requests_and_leaves_auth_global() {
        let id = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa".parse().unwrap();
        for path in [
            "/v1/cartridges",
            "/v1/cartridges/TAPE01/restore",
            "/v1/jobs/job-a",
            "/v1/maintenance/enter",
            "/v1/storage/archive",
        ] {
            assert_eq!(
                scoped_path(Some(id), path),
                format!("/v1/vtls/{id}{}", path.strip_prefix("/v1").unwrap())
            );
            assert_eq!(scoped_path(None, path), path);
        }
        for path in [
            "/auth/login/token",
            "/auth/logout",
            "/v1/vtls",
            "/v1/vtls/aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
        ] {
            assert_eq!(scoped_path(Some(id), path), path);
        }
    }
    #[test]
    fn cli_rejects_invalid_vtl_and_supports_library_discovery() {
        assert!(Cli::try_parse_from(["cold-vtl", "--vtl", "../bad", "media", "list"]).is_err());
        assert!(Cli::try_parse_from(["cold-vtl", "vtl", "list"]).is_ok());
        assert!(
            Cli::try_parse_from([
                "cold-vtl",
                "vtl",
                "show",
                "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
            ])
            .is_ok()
        );
        assert!(
            Cli::try_parse_from([
                "cold-vtl",
                "media",
                "list",
                "--vtl",
                "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
            ])
            .is_ok()
        );
    }
}
