use actix_web::{web, App, HttpServer};
use arc::domain::user::projector::{UserProjector, USERS_VIEW};
use arc::http::controllers::internal_projection_controller::handle_user_projection;
use arc_core::audit::AuditMetadata;
use arc_core::event::{Event, NewEvent};
use arc_core::event_bus::EventBus;
use arc_core::event_store::{EventStore, InMemoryEventStore, VersionCheck};
use arc_core::projection::ProjectionEngine;
use arc_core::read_model_store::{InMemoryReadModelStore, ReadModelStore};
use arc_es_nats::NatsEventBus;
use serde_json::json;
use serial_test::serial;
use std::error::Error;
use std::fs;
use std::io::Read;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::TcpStream;
use tokio::time::sleep;
use uuid::Uuid;

type TestResult = Result<(), Box<dyn Error + Send + Sync>>;

const DOCKER_PROJECT_LABEL: &str = "arc.project=nineties";
const DOCKER_TEST_LABEL: &str = "arc.test=benthos_projection_routing";

struct ChildProcess {
    child: Child,
    _temp_dir: Option<TempDir>,
    docker_container: Option<String>,
}

impl Drop for ChildProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(container) = &self.docker_container {
            remove_docker_container(container);
        }
    }
}

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(prefix: &str) -> Result<Self, Box<dyn Error + Send + Sync>> {
        let path = std::env::temp_dir().join(format!("{prefix}-{}", Uuid::new_v4()));
        fs::create_dir_all(&path)?;
        Ok(Self { path })
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

enum ConnectRuntime {
    Local(&'static str),
    Docker,
}

struct EnvVarGuard {
    name: &'static str,
}

impl EnvVarGuard {
    fn set(name: &'static str, value: &str) -> Self {
        std::env::set_var(name, value);
        Self { name }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        std::env::remove_var(self.name);
    }
}

async fn start_nats() -> Result<Option<(String, ChildProcess)>, Box<dyn Error + Send + Sync>> {
    let port = free_port()?;
    if Command::new("nats-server")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
    {
        let store_dir = TempDir::new("arc-benthos-nats")?;
        let child = Command::new("nats-server")
            .args(["-js", "-p", &port.to_string(), "-sd"])
            .arg(&store_dir.path)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()?;
        return wait_for_nats(port, child, Some(store_dir), None).await;
    }

    if docker_image_available("nats:latest") {
        let container = docker_container_name("nats");
        let child = Command::new("docker")
            .args([
                "run",
                "--rm",
                "--name",
                &container,
                "--label",
                DOCKER_PROJECT_LABEL,
                "--label",
                DOCKER_TEST_LABEL,
                "--network",
                "host",
                "nats:latest",
                "-js",
                "-p",
                &port.to_string(),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()?;
        return wait_for_nats(port, child, None, Some(container)).await;
    }

    eprintln!("skipping Benthos projection routing test: nats-server binary or local nats:latest Docker image not found");
    Ok(None)
}

async fn wait_for_nats(
    port: u16,
    mut child: Child,
    temp_dir: Option<TempDir>,
    docker_container: Option<String>,
) -> Result<Option<(String, ChildProcess)>, Box<dyn Error + Send + Sync>> {
    let url = format!("nats://127.0.0.1:{port}");
    let deadline = Instant::now() + Duration::from_secs(15);

    while Instant::now() < deadline {
        match TcpStream::connect(("127.0.0.1", port)).await {
            Ok(stream) => {
                drop(stream);
                return Ok(Some((
                    url,
                    ChildProcess {
                        child,
                        _temp_dir: temp_dir,
                        docker_container,
                    },
                )));
            }
            Err(connect_error) => {
                if let Some(status) = child.try_wait()? {
                    let stderr = read_child_stderr(&mut child);
                    if let Some(container) = &docker_container {
                        remove_docker_container(container);
                    }
                    return Err(format!(
                        "nats-server exited before accepting connections ({status}): \
                         {connect_error}; stderr: {stderr}"
                    )
                    .into());
                }
                sleep(Duration::from_millis(50)).await;
            }
        }
    }

    let _ = child.kill();
    let _ = child.wait();
    let stderr = read_child_stderr(&mut child);
    if let Some(container) = &docker_container {
        remove_docker_container(container);
    }
    Err(format!(
        "nats-server did not accept connections on 127.0.0.1:{port} within 15 seconds; \
         stderr: {stderr}"
    )
    .into())
}

fn read_child_stderr(child: &mut Child) -> String {
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    let stderr = stderr.trim();
    if stderr.is_empty() {
        "<empty>".to_owned()
    } else {
        stderr.to_owned()
    }
}

fn redpanda_connect_runtime() -> Option<ConnectRuntime> {
    for binary in ["redpanda-connect", "benthos"] {
        if Command::new(binary)
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok()
        {
            return Some(ConnectRuntime::Local(binary));
        }
    }

    docker_image_available("docker.redpanda.com/redpandadata/connect:latest")
        .then_some(ConnectRuntime::Docker)
}

fn docker_image_available(image: &str) -> bool {
    let docker_available = Command::new("docker")
        .arg("version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false);

    docker_available
        && Command::new("docker")
            .args(["image", "inspect", image])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
}

fn docker_container_name(role: &str) -> String {
    format!("arc-nineties-{role}-{}", Uuid::new_v4().simple())
}

fn remove_docker_container(container: &str) {
    let _ = Command::new("docker")
        .args(["rm", "-f", container])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn free_port() -> Result<u16, Box<dyn Error + Send + Sync>> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?.port())
}

fn stream_name() -> String {
    format!("EVENTS_{}", Uuid::new_v4().simple())
}

fn user_registered(id: &str) -> Event {
    Event::new(NewEvent {
        aggregate_type: "User",
        aggregate_id: id,
        sequence: 1,
        event_type: "UserRegistered",
        payload: json!({
            "id": id,
            "name": "Ada",
            "email": "ada@example.test",
            "password_hash": "$argon2$test"
        }),
    })
    .with_audit(AuditMetadata::test_default())
}

async fn start_projection_app(
    event_store: InMemoryEventStore,
    read_model_store: Arc<dyn ReadModelStore>,
) -> Result<(String, actix_web::dev::ServerHandle), Box<dyn Error + Send + Sync>> {
    let mut engine = ProjectionEngine::new(Box::new(event_store));
    engine.register_projector(Box::new(UserProjector::new()), read_model_store, USERS_VIEW);
    let engine = web::Data::new(engine);

    let listener = TcpListener::bind("127.0.0.1:0")?;
    let url = format!("http://{}", listener.local_addr()?);
    let server = HttpServer::new(move || {
        App::new()
            .app_data(engine.clone())
            .service(handle_user_projection)
    })
    .listen(listener)?
    .run();
    let handle = server.handle();
    tokio::spawn(server);

    Ok((url, handle))
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crate parent")
        .parent()
        .expect("workspace root")
        .to_path_buf()
}

fn generate_benthos_config(
    endpoint_url: &str,
    http_port: u16,
) -> Result<TempDir, Box<dyn Error + Send + Sync>> {
    let temp = TempDir::new("arc-benthos-config")?;
    fs::create_dir_all(temp.path.join("config/handlers"))?;
    fs::write(
        temp.path.join("config/handlers/user-projection.yaml"),
        format!(
            r#"name: user-projection
subscribe:
  aggregate_types: [User]
  event_types: [UserRegistered]
delivery:
  type: http
  http:
    url: "{endpoint_url}/users/handle"
    headers:
      Authorization: "Bearer ${{INTERNAL_PROJECTION_TOKEN}}"
"#
        ),
    )?;

    let script = repo_root().join("scripts/generate-benthos-config.mjs");
    let status = Command::new("node")
        .arg(script)
        .current_dir(&temp.path)
        .status()?;
    if !status.success() {
        return Err("failed to generate Benthos test config".into());
    }

    let config_path = temp.path.join("config/benthos/generated/events.yaml");
    let config = fs::read_to_string(&config_path)?;
    fs::write(
        &config_path,
        config.replace(
            "address: 0.0.0.0:4195",
            &format!("address: 127.0.0.1:{http_port}"),
        ),
    )?;

    Ok(temp)
}

fn start_benthos(
    runtime: &ConnectRuntime,
    config_dir: &TempDir,
    nats_url: &str,
    stream: &str,
) -> Result<ChildProcess, Box<dyn Error + Send + Sync>> {
    let consumer = format!("benthos_{}", Uuid::new_v4().simple());
    let config_path = config_dir.path.join("config/benthos/generated/events.yaml");
    let mut docker_container = None;
    let child = match runtime {
        ConnectRuntime::Local(binary) => Command::new(binary)
            .arg("run")
            .arg(config_path)
            .env("NATS_URL", nats_url)
            .env("NATS_STREAM", stream)
            .env("NATS_CONSUMER", &consumer)
            .env("INTERNAL_PROJECTION_TOKEN", "test-projection-token")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?,
        ConnectRuntime::Docker => {
            let container = docker_container_name("benthos");
            let child = Command::new("docker")
                .args([
                    "run",
                    "--rm",
                    "--name",
                    &container,
                    "--label",
                    DOCKER_PROJECT_LABEL,
                    "--label",
                    DOCKER_TEST_LABEL,
                    "--network",
                    "host",
                    "-e",
                    &format!("NATS_URL={nats_url}"),
                    "-e",
                    &format!("NATS_STREAM={stream}"),
                    "-e",
                    &format!("NATS_CONSUMER={consumer}"),
                    "-e",
                    "INTERNAL_PROJECTION_TOKEN=test-projection-token",
                    "-v",
                ])
                .arg(format!("{}:/config/events.yaml:ro", config_path.display()))
                .args([
                    "docker.redpanda.com/redpandadata/connect:latest",
                    "run",
                    "/config/events.yaml",
                ])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()?;
            docker_container = Some(container);
            child
        }
    };
    Ok(ChildProcess {
        child,
        _temp_dir: None,
        docker_container,
    })
}

async fn wait_for_user_row(
    store: &dyn ReadModelStore,
    aggregate_id: &str,
) -> Result<serde_json::Value, Box<dyn Error + Send + Sync>> {
    for _ in 0..60 {
        if let Some(row) = store.get(USERS_VIEW, aggregate_id).await? {
            return Ok(row);
        }
        sleep(Duration::from_millis(100)).await;
    }

    Err("timed out waiting for projected users_view row".into())
}

#[tokio::test]
#[serial]
async fn benthos_routes_nats_event_to_arc_projection_handler() -> TestResult {
    let Some(runtime) = redpanda_connect_runtime() else {
        eprintln!(
            "skipping Benthos projection routing test: redpanda-connect/benthos/docker runtime not found"
        );
        return Ok(());
    };
    if Command::new("node")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_err()
    {
        eprintln!("skipping Benthos projection routing test: node binary not found");
        return Ok(());
    }

    let Some((nats_url, _nats)) = start_nats().await? else {
        return Ok(());
    };

    let _token = EnvVarGuard::set("INTERNAL_PROJECTION_TOKEN", "test-projection-token");
    let read_model_store: Arc<dyn ReadModelStore> = Arc::new(InMemoryReadModelStore::new());
    let event_store = InMemoryEventStore::new();
    let (projection_url, projection_handle) =
        start_projection_app(event_store.clone(), read_model_store.clone()).await?;
    let benthos_http_port = free_port()?;
    let config_dir = generate_benthos_config(&projection_url, benthos_http_port)?;

    let stream = stream_name();
    let bus = NatsEventBus::new(&nats_url, &stream).await?;
    let _benthos = start_benthos(&runtime, &config_dir, &nats_url, &stream)?;
    sleep(Duration::from_millis(500)).await;

    let event = user_registered("user-benthos-projection");
    event_store
        .append_to(
            "User",
            &event.aggregate_id,
            VersionCheck::New,
            vec![event.clone()],
        )
        .await?;
    bus.publish(vec![event.clone()]).await?;

    let row = wait_for_user_row(read_model_store.as_ref(), &event.aggregate_id).await?;
    assert_eq!(row["email"], "ada@example.test");
    assert_eq!(row["version"], 1);

    projection_handle.stop(true).await;
    Ok(())
}
