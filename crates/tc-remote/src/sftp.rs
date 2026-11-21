//! SFTP over russh. Host keys are checked against `~/.ssh/known_hosts`; auth tries the ssh-agent,
//! unencrypted default keys, then a password.

use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use russh::client::{self, Handle};
use russh::keys::{self, HashAlg, PrivateKeyWithHashAlg, PublicKey, PublicKeyOrCertificate};
use russh_sftp::client::SftpSession;
use russh_sftp::client::error::Error as SftpError;
use russh_sftp::protocol::{FileAttributes, OpenFlags, StatusCode};
use tc_core::panel::Entry;
use tc_core::vfs::Remote;
use tc_core::{Error, Result};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::runtime::Runtime;

use crate::{Ask, Prompt, Url, err, join};

const TIMEOUT: Duration = Duration::from_secs(15);

/// Result of the host key check, read after the key exchange.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Known {
    Yes,
    /// Not in known_hosts: the user decides before any credential is sent.
    Unknown,
}

struct Client {
    host: String,
    port: u16,
    seen: Arc<Mutex<Option<(PublicKey, Known)>>>,
}

impl client::Handler for Client {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        key: &PublicKeyOrCertificate,
    ) -> std::result::Result<bool, Self::Error> {
        let PublicKeyOrCertificate::PublicKey { key, .. } = key else {
            return Ok(false);
        };
        // a changed key (Err KeyChanged) is refused like OpenSSH does
        let known = match keys::check_known_hosts(&self.host, self.port, key) {
            Ok(true) => Known::Yes,
            Ok(false) => Known::Unknown,
            Err(_) => return Ok(false),
        };
        if let Ok(mut s) = self.seen.lock() {
            *s = Some((key.clone(), known));
        }
        Ok(true)
    }
}

pub struct Sftp {
    base: String,
    home: String,
    rt: Runtime,
    sftp: SftpSession,
    ssh: Handle<Client>,
}

impl std::fmt::Debug for Sftp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.base)
    }
}

impl Sftp {
    pub fn connect(url: &Url, ask: Ask) -> Result<Self> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .map_err(err)?;
        let user = if url.user.is_empty() {
            std::env::var("USER")
                .or_else(|_| std::env::var("USERNAME"))
                .unwrap_or_default()
        } else {
            url.user.clone()
        };
        let seen = Arc::new(Mutex::new(None));
        let handler = Client {
            host: url.host.clone(),
            port: url.port,
            seen: seen.clone(),
        };
        let config = Arc::new(client::Config {
            keepalive_interval: Some(Duration::from_secs(30)),
            ..Default::default()
        });
        let addr = (url.host.clone(), url.port);
        let mut ssh = rt
            .block_on(async {
                tokio::time::timeout(TIMEOUT, client::connect(config, addr, handler)).await
            })
            .map_err(|_| Error::Remote(format!("{}: connect timed out", url.host)))?
            .map_err(|e| match seen.lock().ok().and_then(|s| s.clone()) {
                None => Error::Remote(format!(
                    "{}: host key refused (changed? see ~/.ssh/known_hosts): {e}",
                    url.host
                )),
                Some(_) => err(e),
            })?;
        let (key, known) = seen
            .lock()
            .ok()
            .and_then(|s| s.clone())
            .ok_or_else(|| Error::Remote("no host key".into()))?;
        if known == Known::Unknown {
            let text = format!(
                "Unknown host {}:{}\n{} {}",
                url.host,
                url.port,
                key.algorithm(),
                key.fingerprint(HashAlg::Sha256)
            );
            if ask(Prompt::HostKey(text)).as_deref() != Some("y") {
                return Err(Error::Cancelled);
            }
            keys::known_hosts::learn_known_hosts(&url.host, url.port, &key).map_err(err)?;
        }
        if !rt.block_on(auth(&mut ssh, &user)) {
            let label = format!("Password for {user}@{}", url.host);
            let pass = ask(Prompt::Password(label)).ok_or(Error::Cancelled)?;
            let ok = rt
                .block_on(ssh.authenticate_password(&user, pass))
                .map_err(err)?
                .success();
            if !ok {
                return Err(Error::Remote("authentication failed".into()));
            }
        }
        let sftp = rt.block_on(async {
            let ch = ssh.channel_open_session().await.map_err(err)?;
            ch.request_subsystem(true, "sftp").await.map_err(err)?;
            SftpSession::new(ch.into_stream()).await.map_err(err)
        })?;
        let home = rt
            .block_on(sftp.canonicalize("."))
            .unwrap_or_else(|_| "/".into());
        let base = Url {
            user,
            ..url.clone()
        }
        .base();
        Ok(Self {
            base,
            home,
            rt,
            sftp,
            ssh,
        })
    }

    fn run<T>(&self, f: impl Future<Output = std::result::Result<T, SftpError>>) -> Result<T> {
        self.rt.block_on(f).map_err(err)
    }
}

/// Agent identities, then unencrypted default key files; true on success.
async fn auth(ssh: &mut Handle<Client>, user: &str) -> bool {
    let rsa = ssh.best_supported_rsa_hash().await.ok().flatten().flatten();
    if let Some(mut agent) = agent().await
        && let Ok(ids) = agent.request_identities().await
    {
        for id in ids {
            let keys::agent::AgentIdentity::PublicKey { key, .. } = id else {
                continue;
            };
            let r = ssh
                .authenticate_publickey_with(user, key, rsa, &mut agent)
                .await;
            if r.is_ok_and(|r| r.success()) {
                return true;
            }
        }
    }
    let Some(home) = std::env::home_dir() else {
        return false;
    };
    // RSA is off: the rsa crate russh needs has the Marvin timing advisory (RUSTSEC-2023-0071)
    for name in ["id_ed25519", "id_ecdsa"] {
        let path: PathBuf = home.join(".ssh").join(name);
        // shortcut: passphrase-protected keys are skipped (use the agent), upgrade with a passphrase prompt
        let Ok(key) = keys::load_secret_key(&path, None) else {
            continue;
        };
        let key = PrivateKeyWithHashAlg::new(Arc::new(key), rsa);
        if ssh
            .authenticate_publickey(user, key)
            .await
            .is_ok_and(|r| r.success())
        {
            return true;
        }
    }
    false
}

#[cfg(unix)]
async fn agent() -> Option<keys::agent::client::AgentClient<tokio::net::UnixStream>> {
    keys::agent::client::AgentClient::connect_env().await.ok()
}

// shortcut: no Windows agent (pageant / OpenSSH pipe), upgrade with AgentClient::connect_named_pipe
#[cfg(not(unix))]
async fn agent() -> Option<keys::agent::client::AgentClient<tokio::io::DuplexStream>> {
    None
}

fn not_found(e: &SftpError) -> bool {
    matches!(e, SftpError::Status(s) if s.status_code == StatusCode::NoSuchFile)
}

fn entry(name: String, a: &FileAttributes, link_dir: Option<bool>) -> Entry {
    Entry {
        name: name.into(),
        is_dir: link_dir.unwrap_or(a.is_dir()),
        is_symlink: a.is_symlink(),
        size: a.size.unwrap_or(0),
        modified: a.modified().ok(),
        link: None,
    }
}

/// Blocking adapters over an async file or channel, driven by the backend's runtime.
struct Blocking<'a, T> {
    rt: &'a Runtime,
    file: T,
}

impl<T: AsyncRead + Unpin> Read for Blocking<'_, T> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.rt.block_on(self.file.read(buf))
    }
}

impl<T: AsyncWrite + Unpin> Write for Blocking<'_, T> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.rt.block_on(self.file.write(buf))
    }

    fn flush(&mut self) -> io::Result<()> {
        self.rt.block_on(self.file.flush())
    }
}

impl Remote for Sftp {
    fn url(&self) -> &str {
        &self.base
    }

    fn home(&self) -> String {
        self.home.clone()
    }

    fn list(&self, dir: &str) -> Result<Vec<Entry>> {
        let items = self.run(self.sftp.read_dir(dir))?;
        let mut out = Vec::new();
        for i in items {
            let (name, meta) = (i.file_name(), i.metadata());
            if matches!(name.as_str(), "." | "..") {
                continue;
            }
            // symlinks: follow once to know whether Enter opens them as a directory
            let link_dir = meta.is_symlink().then(|| {
                self.rt
                    .block_on(self.sftp.metadata(join(dir, &name)))
                    .is_ok_and(|m| m.is_dir())
            });
            out.push(entry(name, &meta, link_dir));
        }
        Ok(out)
    }

    fn stat(&self, path: &str) -> Result<Option<Entry>> {
        match self.rt.block_on(self.sftp.metadata(path)) {
            Ok(m) => Ok(Some(entry(crate::split(path).1.into(), &m, None))),
            Err(e) if not_found(&e) => Ok(None),
            Err(e) => Err(err(e)),
        }
    }

    fn lstat(&self, path: &str) -> Result<Option<Entry>> {
        match self.rt.block_on(self.sftp.symlink_metadata(path)) {
            Ok(m) => Ok(Some(entry(crate::split(path).1.into(), &m, None))),
            Err(e) if not_found(&e) => Ok(None),
            Err(e) => Err(err(e)),
        }
    }

    fn exec(&self, cmd: &str, f: &mut dyn FnMut(&mut dyn Read) -> Result<()>) -> Result<()> {
        let ch = self.rt.block_on(async {
            let ch = self.ssh.channel_open_session().await?;
            ch.exec(true, cmd).await.map(|()| ch)
        });
        let mut b = Blocking {
            rt: &self.rt,
            file: ch.map_err(err)?.into_stream(),
        };
        let r = f(&mut b);
        // dropping the stream spawns its channel close, which panics outside the runtime
        let _rt = self.rt.enter();
        drop(b);
        r
    }

    fn read(&self, path: &str, f: &mut dyn FnMut(&mut dyn Read) -> Result<()>) -> Result<()> {
        let file = self.run(self.sftp.open(path))?;
        let mut b = Blocking { rt: &self.rt, file };
        let r = f(&mut b);
        let _ = self.rt.block_on(b.file.shutdown());
        r
    }

    fn write_new(&self, path: &str, f: &mut dyn FnMut(&mut dyn Write) -> Result<()>) -> Result<()> {
        let flags = OpenFlags::CREATE | OpenFlags::EXCLUDE | OpenFlags::WRITE;
        let file = self.run(self.sftp.open_with_flags(path, flags))?;
        let mut b = Blocking { rt: &self.rt, file };
        let r = f(&mut b);
        let closed = self.rt.block_on(b.file.shutdown()).map_err(err);
        r.and(closed)
    }

    fn mkdir(&self, path: &str) -> Result<()> {
        self.run(self.sftp.create_dir(path))
    }

    fn remove_file(&self, path: &str) -> Result<()> {
        self.run(self.sftp.remove_file(path))
    }

    fn remove_dir(&self, path: &str) -> Result<()> {
        self.run(self.sftp.remove_dir(path))
    }

    fn rename(&self, from: &str, to: &str) -> Result<()> {
        self.run(self.sftp.rename(from, to))
    }
}
