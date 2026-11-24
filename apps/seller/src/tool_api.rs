use anyhow::{ensure, Context, Result};
use axum::Router;
use hyper::server::conn::http1;
use hyper_util::{rt::TokioIo, service::TowerToHyperService};
use std::{fs, os::unix::fs::{FileTypeExt, OpenOptionsExt, PermissionsExt}, path::{Path, PathBuf}};
use tokio::{
    net::{TcpListener, UnixListener, UnixStream},
    sync::watch,
    task::{JoinHandle, JoinSet},
};

const SOCKET: &str = "model.sock";
const CONTAINER_DIR: &str = "/modelcat-api";

pub fn serve(
    dir: &Path,
    key: &str,
    routes: Router,
    mut shutdown: watch::Receiver<bool>,
) -> Result<JoinHandle<()>> {
    let socket = dir.join(SOCKET);
    if let Ok(metadata) = fs::symlink_metadata(&socket) {
        ensure!(metadata.file_type().is_socket(), "tool API path is not a socket");
        fs::remove_file(&socket)?;
    }
    let listener = UnixListener::bind(&socket).context("cannot bind the tool model API socket")?;
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))?;
    {
        use std::io::Write;
        let mut file = fs::OpenOptions::new().create(true).write(true).truncate(true).mode(0o600)
            .open(dir.join("model.key"))?;
        file.write_all(key.as_bytes())?;
    }
    Ok(tokio::spawn(async move {
        let _socket = SocketPath(socket);
        let mut connections = JoinSet::new();
        loop {
            tokio::select! {
                biased;
                _ = shutdown.changed() => break,
                _ = connections.join_next(), if !connections.is_empty() => {},
                accepted = listener.accept() => {
                    let Ok((stream, _)) = accepted else { break };
                    let routes = routes.clone();
                    let mut shutdown = shutdown.clone();
                    connections.spawn(async move {
                        let connection = http1::Builder::new().serve_connection(
                            TokioIo::new(stream), TowerToHyperService::new(routes)
                        );
                        tokio::pin!(connection);
                        tokio::select! {
                            _ = &mut connection => {},
                            _ = shutdown.changed() => {
                                connection.as_mut().graceful_shutdown();
                                let _ = connection.await;
                            }
                        }
                    });
                }
            }
        }
        drop(listener);
        while connections.join_next().await.is_some() {}
    }))
}

struct SocketPath(PathBuf);

impl Drop for SocketPath {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

pub async fn relay() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:19702").await?;
    let socket = Path::new(CONTAINER_DIR).join(SOCKET);
    loop {
        let (mut client, _) = listener.accept().await?;
        let socket = socket.clone();
        tokio::spawn(async move {
            if let Ok(mut upstream) = UnixStream::connect(socket).await {
                let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
            }
        });
    }
}
