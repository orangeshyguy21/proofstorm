//! An ordinary host HTTP client, kept on one connection across cell edits.
use crate::GateContext;
use anyhow::{Context, Result, ensure};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    process::{Child, Stdio},
    time::Duration,
};

pub struct ExternalClient {
    forward: Child,
    stream: BufReader<TcpStream>,
}
impl ExternalClient {
    pub fn connect(context: &GateContext, namespace: &str) -> Result<Self> {
        let port = TcpListener::bind("127.0.0.1:0")?.local_addr()?.port();
        let mut forward = context
            .kubectl
            .command(&[
                "port-forward",
                "-n",
                namespace,
                "service/mint",
                &format!("{port}:3338"),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        for _ in 0..50 {
            if let Ok(stream) = TcpStream::connect(("127.0.0.1", port)) {
                stream.set_read_timeout(Some(Duration::from_secs(5)))?;
                stream.set_write_timeout(Some(Duration::from_secs(5)))?;
                return Ok(Self {
                    forward,
                    stream: BufReader::new(stream),
                });
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = forward.kill();
        let _ = forward.wait();
        anyhow::bail!("external client port-forward did not start")
    }
    pub fn check(&mut self) -> Result<()> {
        self.stream
            .get_mut()
            .write_all(b"GET /v1/info HTTP/1.1\r\nHost: mint\r\nConnection: keep-alive\r\n\r\n")?;
        let mut line = String::new();
        self.stream.read_line(&mut line)?;
        ensure!(
            line.starts_with("HTTP/1.1 200"),
            "external connection failed: {line}"
        );
        let mut length = None;
        loop {
            line.clear();
            ensure!(
                self.stream.read_line(&mut line)? > 0,
                "external connection closed"
            );
            if line == "\r\n" {
                break;
            }
            if let Some((key, value)) = line.split_once(':') {
                if key.eq_ignore_ascii_case("content-length") {
                    length = Some(value.trim().parse::<usize>()?);
                }
            }
        }
        let length = length.context("mint info must provide a content length")?;
        ensure!(length < 65536, "unexpected mint info response size");
        let mut body = vec![0; length];
        self.stream.read_exact(&mut body)?;
        let info: serde_json::Value = serde_json::from_slice(&body)?;
        ensure!(info.get("nuts").is_some(), "invalid mint info");
        Ok(())
    }
}
impl Drop for ExternalClient {
    fn drop(&mut self) {
        let _ = self.forward.kill();
        let _ = self.forward.wait();
    }
}
