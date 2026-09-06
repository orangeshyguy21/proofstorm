//! Explicit local application access, independent of activity sessions.
use crate::{Error, lab::Labs};
use k8s_openapi::api::core::v1::{Pod, Service};
use kube::{Api, ResourceExt, api::ListParams};
use proofstorm_core::{Capability, LabInstance, PublishedRevision};
use proofstorm_store::LabHandlePhase;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::{net::Ipv4Addr, path::Path, time::Duration};
use tokio::{
    net::{TcpListener, TcpStream},
    task::JoinSet,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Authentication {
    None,
    Basic,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ConnectionDescriptor {
    pub lab: String,
    pub component: String,
    pub endpoint: String,
    pub protocol: String,
    pub url: String,
    pub authentication: Authentication,
    pub access: String,
    pub fault_path: String,
    pub lifetime: String,
}

/// Contains no credential material; safe for graph/status output.
pub struct Connection {
    pub descriptor: ConnectionDescriptor,
    listener: TcpListener,
    labs: Labs,
    instance: LabInstance,
    target_port: u16,
}

pub fn endpoint(
    revision: &PublishedRevision,
    component: &str,
    endpoint: &str,
) -> Result<(u16, Authentication), Error> {
    let component = revision
        .lab
        .components
        .iter()
        .find(|c| c.id == component)
        .ok_or_else(|| {
            Error::problem("component_not_found", "component is not part of this lab")
        })?;
    let authentication = match (component.implementation.as_str(), endpoint) {
        ("bitcoin-core", "rpc") => Authentication::Basic,
        ("cdk" | "cdk-ldk" | "cdk-bdk" | "nutshell", "http") => Authentication::None,
        _ => {
            return Err(Error::problem(
                "connection_unsupported",
                "local connections currently support mint http and bitcoin-core rpc; local-only wallet and CLN sockets are not exported",
            ));
        }
    };
    let port = proofstorm_kube::component_ports(component)
        .get(endpoint)
        .copied()
        .ok_or_else(|| {
            Error::problem(
                "endpoint_not_found",
                "endpoint has no advertised service port",
            )
        })?;
    Ok((port, authentication))
}

impl Labs {
    pub async fn connect(
        &self,
        name: &str,
        component: &str,
        endpoint_name: &str,
        local_port: u16,
    ) -> Result<Connection, Error> {
        self.store
            .authorize(&self.workspace, &self.principal, Capability::LabConnect)?;
        let lab = self
            .store
            .lab_handle(&self.workspace, &self.principal, name)?;
        if lab.phase != LabHandlePhase::Open {
            return Err(Error::problem(
                "connection_refused",
                "connection requires an open lab",
            ));
        }
        let (instance, revision) = self.store.operation_context(
            &self.workspace,
            &self.principal,
            &lab.instance_id,
            Capability::LabConnect,
        )?;
        let (target_port, authentication) = endpoint(&revision, component, endpoint_name)?;
        // Verify a real service and ready target before advertising a local address.
        resolve_pod(&self.runtime, &instance, component, target_port).await?;
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, local_port))
            .await
            .map_err(io_error)?;
        let address = listener.local_addr().map_err(io_error)?;
        Ok(Connection {descriptor:ConnectionDescriptor {
            lab:name.into(),component:component.into(),endpoint:endpoint_name.into(),protocol:"http".into(),url:format!("http://{address}"),authentication,
            access:"loopback_only".into(),fault_path:"kubernetes_tunnel_bypasses_lab_network_policies".into(),lifetime:"until this connection process stops or the lab closes; existing TCP sessions may fail on component restart".into(),
        },listener,labs:self.clone(),instance,target_port})
    }
}

impl Connection {
    /// Write app configuration only to a newly created owner-only file.
    /// The caller must retain this Connection until its serving task ends.
    pub fn write_config(&self, path: &Path) -> Result<(), Error> {
        use std::io::Write;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(path).map_err(io_error)?;
        let mut config = serde_json::json!({"url":self.descriptor.url,"protocol":self.descriptor.protocol,"authentication":self.descriptor.authentication});
        if self.descriptor.authentication == Authentication::Basic {
            config["username"] = serde_json::json!(proofstorm_kube::BITCOIN_RPC_USER);
            config["password"] = serde_json::json!(proofstorm_kube::BITCOIN_RPC_PASSWORD);
        }
        let result = file
            .write_all(
                &serde_json::to_vec_pretty(&config)
                    .map_err(|e| Error::failure(e.to_string(), None))?,
            )
            .and_then(|()| file.sync_all());
        if result.is_err() {
            let _ = std::fs::remove_file(path);
        }
        result.map_err(io_error)
    }

    /// Bounded concurrent TCP forwarding. Dropping this future aborts accepted
    /// connections; each connection re-resolves the current workload after restart.
    pub async fn serve(self) -> Result<(), Error> {
        let mut tasks = JoinSet::new();
        let mut health = tokio::time::interval(Duration::from_secs(1));
        loop {
            tokio::select! {
                accepted=self.listener.accept(), if tasks.len()<32 => {
                    let (socket,_)=accepted.map_err(io_error)?;
                    let runtime=self.labs.runtime.clone();
                    let instance=self.instance.clone();
                    let component=self.descriptor.component.clone();
                    let port=self.target_port;
                    tasks.spawn(async move {forward(runtime,instance,component,port,socket).await});
                },
                completed=tasks.join_next(), if !tasks.is_empty() => {
                    if let Some(result)=completed {
                        match result {Ok(Ok(()))=>{},Ok(Err(error))=>eprintln!("connection ended: {error}"),Err(error)=>eprintln!("connection task ended: {error}")}
                    }
                },
                _=health.tick()=> {
                    self.labs.store.authorize(&self.labs.workspace,&self.labs.principal,Capability::LabConnect)?;
                    let lab=self.labs.store.lab_handle(&self.labs.workspace,&self.labs.principal,&self.descriptor.lab)?;
                    if lab.phase!=LabHandlePhase::Open || lab.instance_id!=self.instance.id {return Ok(());}
                    let labs=Api::<proofstorm_kube::ProofstormLab>::namespaced(self.labs.runtime.client.clone(),&self.labs.runtime.control_namespace);
                    let Some(lab)=tokio::time::timeout(Duration::from_secs(5), labs.get_opt(&self.instance.resource_name)).await.map_err(|_| Error::problem("connection_health_timeout", "runtime health check timed out; connection closed"))?? else {return Ok(());};
                    if proofstorm_kube::require_open_lab(&lab).is_err() {return Ok(());}
                }
            }
        }
    }
}

async fn resolve_pod(
    runtime: &crate::Runtime,
    instance: &LabInstance,
    component: &str,
    port: u16,
) -> Result<String, Error> {
    let namespace = proofstorm_kube::instance_namespace(&instance.instance_key);
    let services = Api::<Service>::namespaced(runtime.client.clone(), &namespace);
    let service = services.get(component).await?;
    if service
        .metadata
        .labels
        .as_ref()
        .and_then(|l| l.get(proofstorm_kube::INSTANCE_LABEL))
        != Some(&instance.instance_key)
    {
        return Err(Error::problem(
            "connection_identity_mismatch",
            "service does not belong to the selected lab",
        ));
    }
    if !service
        .spec
        .as_ref()
        .and_then(|s| s.ports.as_ref())
        .is_some_and(|ports| ports.iter().any(|p| p.port == i32::from(port)))
    {
        return Err(Error::problem(
            "connection_port_mismatch",
            "service no longer advertises the selected port",
        ));
    }
    let pods = Api::<Pod>::namespaced(runtime.client.clone(), &namespace)
        .list(&ListParams::default().labels(&format!(
            "proofstorm.dev/instance={},proofstorm.dev/component={component}",
            instance.instance_key
        )))
        .await?;
    let pod = pods
        .items
        .into_iter()
        .find(|p| {
            p.metadata.deletion_timestamp.is_none()
                && p.status
                    .as_ref()
                    .and_then(|s| s.conditions.as_ref())
                    .is_some_and(|conditions| {
                        conditions
                            .iter()
                            .any(|c| c.type_ == "Ready" && c.status == "True")
                    })
        })
        .ok_or_else(|| {
            Error::problem(
                "component_unavailable",
                "component has no ready workload; retry after recovery",
            )
        })?;
    Ok(pod.name_any())
}

struct ForwardGuard(kube::api::Portforwarder);
impl Drop for ForwardGuard {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn forward(
    runtime: crate::Runtime,
    instance: LabInstance,
    component: String,
    port: u16,
    mut socket: TcpStream,
) -> Result<(), Error> {
    let pod = resolve_pod(&runtime, &instance, &component, port).await?;
    let pods = Api::<Pod>::namespaced(
        runtime.client.clone(),
        &proofstorm_kube::instance_namespace(&instance.instance_key),
    );
    let mut forwarder = ForwardGuard(pods.portforward(&pod, &[port]).await?);
    let mut stream = forwarder.0.take_stream(port).ok_or_else(|| {
        Error::problem(
            "connection_stream_missing",
            "runtime did not open the requested stream",
        )
    })?;
    tokio::io::copy_bidirectional(&mut socket, &mut stream)
        .await
        .map_err(io_error)?;
    Ok(())
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "map_err consumes the I/O error"
)]
fn io_error(error: std::io::Error) -> Error {
    Error::failure(
        error.to_string(),
        Some(serde_json::json!({"code":"connection_io_failure"})),
    )
}
