// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use addr::TargetAddr;
use anyhow::{bail, Context, Result};
use discovery::desc::Description;
use discovery::query::TargetInfoQuery;
use discovery::{DiscoverySources, TargetEvent, TargetHandle, TargetState};
use errors::FfxError;
use ffx_config::EnvironmentContext;
use fidl_fuchsia_developer_ffx::{self as ffx};
use fidl_fuchsia_developer_remotecontrol::{IdentifyHostResponse, RemoteControlProxy};
use fuchsia_async::TimeoutExt;
use futures::future::join_all;
use futures::{FutureExt, StreamExt};
use itertools::Itertools;
use netext::IsLocalAddr;
use std::cmp::Ordering;
use std::collections::HashSet;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use crate::connection::Connection;
use crate::ssh_connector::SshConnector;

const CONFIG_TARGET_SSH_TIMEOUT: &str = "target.host_pipe_ssh_timeout";
const CONFIG_LOCAL_DISCOVERY_TIMEOUT: &str = "discovery.timeout";
const SSH_PORT_DEFAULT: u16 = 22;
const DEFAULT_SSH_TIMEOUT_MS: u64 = 10000;

#[cfg(test)]
use {mockall::mock, mockall::predicate::*};

/// Check if daemon discovery is disabled, resolving locally if so.
pub async fn maybe_locally_resolve_target_spec(
    target_spec: Option<String>,
    env_context: &EnvironmentContext,
) -> Result<Option<String>> {
    if crate::is_discovery_enabled(env_context).await {
        Ok(target_spec)
    } else {
        locally_resolve_target_spec(target_spec, &QueryResolver::default(), env_context).await
    }
}

/// Attempts to resolve the query into an explicit string query that can be
/// passed to the daemon. If already an address or serial number, just return
/// it. Otherwise, perform discovery to find the address or serial #. Returns
/// Some(_) if a target has been found, None otherwise.
async fn locally_resolve_target_spec<T: QueryResolverT>(
    target_spec: Option<String>,
    resolver: &T,
    env_context: &EnvironmentContext,
) -> Result<Option<String>> {
    let query = TargetInfoQuery::from(target_spec.clone());
    let explicit_spec = match query {
        TargetInfoQuery::Addr(addr) if addr.port() != 0 => format!("{addr}"),
        TargetInfoQuery::Serial(sn) => format!("serial:{sn}"),
        _ => {
            let resolution = resolver.resolve_single_target(&target_spec, env_context).await?;
            tracing::debug!(
                "Locally resolved target '{target_spec:?}' to {:?}",
                resolution.discovered
            );
            resolution.target.to_spec()
        }
    };

    Ok(Some(explicit_spec))
}

pub trait QueryResolverT {
    #[allow(async_fn_in_trait)]
    async fn resolve_target_query(
        &self,
        query: TargetInfoQuery,
        ctx: &EnvironmentContext,
    ) -> Result<Vec<TargetHandle>>;

    #[allow(async_fn_in_trait)]
    async fn try_resolve_manual_target(
        &self,
        name: &str,
        ctx: &EnvironmentContext,
    ) -> Result<Option<Resolution>>;

    #[allow(async_fn_in_trait)]
    async fn resolve_target_address(
        &self,
        target_spec: &Option<String>,
        ctx: &EnvironmentContext,
    ) -> Result<Resolution> {
        let query = TargetInfoQuery::from(target_spec.clone());
        if let TargetInfoQuery::Addr(a) = query {
            return Ok(Resolution::from_addr(a));
        }
        let res = self.resolve_single_target(&target_spec, ctx).await?;
        let target_spec_info = target_spec.clone().unwrap_or_else(|| "<unspecified>".to_owned());
        tracing::debug!("resolved target spec {target_spec_info} to address {:?}", res.addr());
        Ok(res)
    }

    #[allow(async_fn_in_trait)]
    async fn resolve_single_target(
        &self,
        target_spec: &Option<String>,
        env_context: &EnvironmentContext,
    ) -> Result<Resolution, anyhow::Error> {
        let target_spec_info = target_spec.clone().unwrap_or_else(|| "<unspecified>".to_owned());
        let query = TargetInfoQuery::from(target_spec.clone());
        if let TargetInfoQuery::NodenameOrSerial(ref s) = query {
            match self.try_resolve_manual_target(s, env_context).await {
                Err(e) => {
                    tracing::debug!("Failed to resolve target {s} as manual target: {e:?}");
                }
                Ok(Some(res)) => return Ok(res),
                _ => (), // Keep going
            }
        }
        let mut handles = self.resolve_target_query(query, env_context).await?;
        if handles.len() == 0 {
            return Err(anyhow::anyhow!(
                "unable to resolve address for target '{target_spec_info}'"
            ));
        }
        if handles.len() > 1 {
            return Err(FfxError::DaemonError {
                err: ffx::DaemonError::TargetAmbiguous,
                target: target_spec.clone(),
            }
            .into());
        }
        Ok(Resolution::from_target_handle(handles.remove(0))?)
    }
}

#[cfg(test)]
mock! {
    QueryResolverT{}
    impl QueryResolverT for QueryResolverT {
        async fn resolve_target_query(
            &self,
            query: TargetInfoQuery,
            ctx: &EnvironmentContext,
        ) -> Result<Vec<TargetHandle>>;
        async fn try_resolve_manual_target(
            &self,
            name: &str,
            ctx: &EnvironmentContext,
        ) -> Result<Option<Resolution>>;
    }
}

/// Attempts to resolve a TargetInfoQuery into a list of discovered targets.
/// Useful when multiple results are reasonable (e.g. from `ffx target list`)
pub async fn resolve_target_query(
    query: TargetInfoQuery,
    ctx: &EnvironmentContext,
) -> Result<Vec<TargetHandle>> {
    resolve_target_query_with_sources(
        query,
        ctx,
        DiscoverySources::MDNS
            | DiscoverySources::USB
            | DiscoverySources::MANUAL
            | DiscoverySources::EMULATOR,
    )
    .await
}

struct RetrievedTargetInfo {
    rcs_state: ffx::RemoteControlState,
    product_config: Option<String>,
    board_config: Option<String>,
    ssh_address: Option<SocketAddr>,
}

impl Default for RetrievedTargetInfo {
    fn default() -> Self {
        Self {
            rcs_state: ffx::RemoteControlState::Unknown,
            product_config: None,
            board_config: None,
            ssh_address: None,
        }
    }
}

async fn try_get_target_info(
    addr: addr::TargetAddr,
    context: &EnvironmentContext,
) -> Result<(Option<String>, Option<String>), crate::KnockError> {
    let connector =
        SshConnector::new(addr.into(), context).await.context("making ssh connector")?;
    let conn = Connection::new(connector).await.context("making direct connection")?;
    let rcs = conn.rcs_proxy().await.context("getting RCS proxy")?;
    let (pc, bc) = match rcs.identify_host().await {
        Ok(Ok(id_result)) => (id_result.product_config, id_result.board_config),
        _ => (None, None),
    };
    Ok((pc, bc))
}

impl RetrievedTargetInfo {
    async fn get(context: &EnvironmentContext, addrs: &[addr::TargetAddr]) -> Result<Self> {
        let ssh_timeout: u64 =
            ffx_config::get(CONFIG_TARGET_SSH_TIMEOUT).await.unwrap_or(DEFAULT_SSH_TIMEOUT_MS);
        let ssh_timeout = Duration::from_millis(ssh_timeout);
        for addr in addrs {
            tracing::debug!("Trying to make a connection to {addr:?}");

            // If the port is 0, we treat that as the default ssh port.
            let mut addr = *addr;
            if addr.port() == 0 {
                addr.set_port(SSH_PORT_DEFAULT);
            }

            match try_get_target_info(addr, context)
                .on_timeout(ssh_timeout, || {
                    Err(crate::KnockError::NonCriticalError(anyhow::anyhow!(
                        "knock_rcs() timed out"
                    )))
                })
                .await
            {
                Ok((product_config, board_config)) => {
                    return Ok(Self {
                        rcs_state: ffx::RemoteControlState::Up,
                        product_config,
                        board_config,
                        ssh_address: Some(addr.into()),
                    });
                }
                Err(crate::KnockError::NonCriticalError(e)) => {
                    tracing::debug!("Could not connect to {addr:?}: {e:?}");
                    continue;
                }
                e => {
                    tracing::debug!("Got error {e:?} when trying to connect to {addr:?}");
                    return Ok(Self {
                        rcs_state: ffx::RemoteControlState::Unknown,
                        product_config: None,
                        board_config: None,
                        ssh_address: None,
                    });
                }
            }
        }
        Ok(Self {
            rcs_state: ffx::RemoteControlState::Down,
            product_config: None,
            board_config: None,
            ssh_address: None,
        })
    }
}

async fn get_handle_info(
    handle: TargetHandle,
    context: &EnvironmentContext,
) -> Result<ffx::TargetInfo> {
    let (target_state, addresses) = match handle.state {
        TargetState::Unknown => (ffx::TargetState::Unknown, None),
        TargetState::Product(target_addrs) => (ffx::TargetState::Product, Some(target_addrs)),
        TargetState::Fastboot(_) => (ffx::TargetState::Fastboot, None),
        TargetState::Zedboot => (ffx::TargetState::Zedboot, None),
    };
    let RetrievedTargetInfo { rcs_state, product_config, board_config, ssh_address } =
        if let Some(ref target_addrs) = addresses {
            RetrievedTargetInfo::get(context, target_addrs).await?
        } else {
            RetrievedTargetInfo::default()
        };
    let addresses =
        addresses.map(|ta| ta.into_iter().map(|x| x.into()).collect::<Vec<ffx::TargetAddrInfo>>());
    Ok(ffx::TargetInfo {
        nodename: handle.node_name,
        addresses,
        rcs_state: Some(rcs_state),
        target_state: Some(target_state),
        board_config,
        product_config,
        ssh_address: ssh_address.map(|a| TargetAddr::from(a).into()),
        ..Default::default()
    })
}

pub async fn resolve_target_query_to_info(
    query: TargetInfoQuery,
    ctx: &EnvironmentContext,
) -> Result<Vec<ffx::TargetInfo>> {
    let handles = resolve_target_query(query, ctx).await?;
    let targets =
        join_all(handles.into_iter().map(|t| async { get_handle_info(t, ctx).await })).await;
    targets.into_iter().collect::<Result<Vec<ffx::TargetInfo>>>()
}

pub async fn resolve_target_query_with(
    query: TargetInfoQuery,
    ctx: &EnvironmentContext,
    usb: bool,
    mdns: bool,
) -> Result<Vec<TargetHandle>> {
    let mut sources = DiscoverySources::MANUAL | DiscoverySources::EMULATOR;
    if usb {
        sources = sources | DiscoverySources::USB;
    }
    if mdns {
        sources = sources | DiscoverySources::MDNS;
    }
    resolve_target_query_with_sources(query, ctx, sources).await
}

async fn resolve_target_query_with_sources(
    query: TargetInfoQuery,
    ctx: &EnvironmentContext,
    sources: DiscoverySources,
) -> Result<Vec<TargetHandle>> {
    // Get nodename, in case we're trying to find an exact match
    QueryResolver::new(sources).resolve_target_query(query, ctx).await
}

/// Attempts to resolve the query into a target's ssh-able address. It is an error
/// if it the query doesn't match exactly one target.
// Perhaps refactor as connect_to_target() -> Result<Connection>, since that seems
// to be the only way this function is used?
pub async fn resolve_target_address(
    target_spec: &Option<String>,
    ctx: &EnvironmentContext,
) -> Result<Resolution> {
    QueryResolver::default().resolve_target_address(target_spec, ctx).await
}
struct QueryResolver {
    sources: DiscoverySources,
}

impl Default for QueryResolver {
    fn default() -> Self {
        Self::new(DiscoverySources::all())
    }
}

impl QueryResolver {
    fn new(sources: DiscoverySources) -> Self {
        Self { sources }
    }
}

fn query_matches_handle(query: &TargetInfoQuery, h: &TargetHandle) -> bool {
    match query {
        TargetInfoQuery::NodenameOrSerial(ref s) => {
            if let Some(nn) = &h.node_name {
                if nn == s {
                    return true;
                }
            }
            if let TargetState::Fastboot(fts) = &h.state {
                if fts.serial_number == *s {
                    return true;
                }
            }
        }
        TargetInfoQuery::Serial(ref s) => {
            if let TargetState::Fastboot(fts) = &h.state {
                if fts.serial_number == *s {
                    return true;
                }
            }
        }
        TargetInfoQuery::Addr(ref sa) => {
            if let TargetState::Product(addrs) = &h.state {
                if addrs.iter().any(|a| a.ip() == sa.ip()) {
                    return true;
                }
            }
        }
        TargetInfoQuery::First => {}
    }
    false
}

// Descriptions are used for matching against a TargetInfoQuery
fn handle_to_description(handle: &TargetHandle) -> Description {
    let (addresses, serial) = match &handle.state {
        TargetState::Product(target_addr) => (target_addr.clone(), None),
        TargetState::Fastboot(discovery::FastbootTargetState { serial_number: sn, .. }) => {
            (vec![], Some(sn.clone()))
        }
        _ => (vec![], None),
    };
    Description { nodename: handle.node_name.clone(), addresses, serial, ..Default::default() }
}

impl QueryResolverT for QueryResolver {
    async fn resolve_target_query(
        &self,
        query: TargetInfoQuery,
        ctx: &EnvironmentContext,
    ) -> Result<Vec<TargetHandle>> {
        // Get nodename, in case we're trying to find an exact match
        let query_clone = query.clone();
        let filter = move |handle: &TargetHandle| {
            let description = handle_to_description(handle);
            query_clone.match_description(&description)
        };
        let emu_instance_root: PathBuf = ctx.get(emulator_instance::EMU_INSTANCE_ROOT_DIR)?;
        let stream =
            discovery::wait_for_devices(filter, Some(emu_instance_root), true, false, self.sources)
                .await?;
        let discovery_delay = ctx.get(CONFIG_LOCAL_DISCOVERY_TIMEOUT).unwrap_or(2000);
        let delay = Duration::from_millis(discovery_delay);

        // This is tricky. We want the stream to complete immediately if we find
        // a target whose name/serial matches the query exactly. Otherwise, run
        // until the timer fires.
        // We can't use `Stream::wait_until()`, because that would require us
        // to return true for the found item, and false for the _next_ item.
        // But there may be no next item, so the stream would end up waiting
        // for the timer anyway. Instead, we create two futures: the timer, and
        // one that is ready when we find the target we're looking for. Then we
        // use `Stream::take_until()`, waiting until _either_ of those futures
        // is ready (by using `race()`). The only remaining tricky part is that
        // we need to examine each event to determine if it matches what we're
        // looking for -- so we interpose a closure via `Stream::map()` that
        // examines each item, before returning them unmodified.
        // Oh, and once we've got a set of results, if any of them are Err,
        // cause the whole thing to be an Err.  We could stop the race early in
        // case of failure by using the same technique, I suppose.
        let target_events: Result<Vec<TargetEvent>> = {
            let timer = fuchsia_async::Timer::new(delay).fuse();
            let found_target_event = async_utils::event::Event::new();
            let found_it = found_target_event.wait().fuse();
            let results: Vec<Result<_>> = stream
                .map(move |ev| {
                    if let Ok(TargetEvent::Added(ref h)) = ev {
                        if query_matches_handle(&query, h) {
                            found_target_event.signal();
                        }
                        ev
                    } else {
                        unreachable!()
                    }
                })
                .take_until(futures_lite::future::race(timer, found_it))
                .collect()
                .await;
            // Fail if any results are Err
            tracing::debug!("target events results: {results:?}");
            let r: Result<Vec<_>> = results.into_iter().collect();
            r
        };

        // Extract handles from Added events
        let added_handles: Vec<_> = target_events?
            .into_iter()
            .map(|e| {
                if let discovery::TargetEvent::Added(handle) = e {
                    handle
                } else {
                    unreachable!()
                }
            })
            .collect();

        // Sometimes libdiscovery returns multiple Added events for the same target (I think always
        // user emulators). The information is always the same, let's just extract the unique entries.
        let unique_handles = added_handles.into_iter().collect::<HashSet<_>>();
        Ok(unique_handles.into_iter().collect())
    }

    #[allow(async_fn_in_trait)]
    async fn try_resolve_manual_target(
        &self,
        name: &str,
        ctx: &EnvironmentContext,
    ) -> Result<Option<Resolution>> {
        // This is something that is often mocked for testing. An improvement here would be to use the
        // environment context for locating manual targets.
        let finder = manual_targets::Config::default();
        let ssh_timeout: u64 =
            ffx_config::get(CONFIG_TARGET_SSH_TIMEOUT).await.unwrap_or(DEFAULT_SSH_TIMEOUT_MS);
        let ssh_timeout = Duration::from_millis(ssh_timeout);
        let mut res = None;
        for t in manual_targets::watcher::parse_manual_targets(&finder).await.into_iter() {
            let addr = t.addr();
            let mut resolution = Resolution::from_addr(addr);
            let identify = resolution
                .identify(ctx)
                .on_timeout(ssh_timeout, || {
                    Err(anyhow::anyhow!(
                        "timeout after {ssh_timeout:?} identifying manual target {t:?}"
                    ))
                })
                .await?;

            if identify.nodename == Some(String::from(name)) {
                res = Some(resolution);
            }
        }
        Ok(res)
    }
}

// Group the information collected when resolving the address. (This is
// particularly important for the rcs_proxy, which we may need when resolving
// a manual target -- we don't want make an RCS connection just to resolve the
// name, drop it, then re-establish it later.)
enum ResolutionTarget {
    Addr(SocketAddr),
    Serial(String),
}

impl ResolutionTarget {
    // If the target is a product, pull out the "best" address from the
    // target, and return it.
    fn from_target_handle(target: &TargetHandle) -> Result<ResolutionTarget> {
        match &target.state {
            TargetState::Product(ref addresses) => {
                if addresses.is_empty() {
                    bail!("Target discovered but does not contain addresses: {target:?}");
                }
                let mut addrs_sorted = addresses
                    .into_iter()
                    .map(SocketAddr::from)
                    .sorted_by(|a1, a2| {
                        match (a1.ip().is_link_local_addr(), a2.ip().is_link_local_addr()) {
                            (true, true) | (false, false) => Ordering::Equal,
                            (true, false) => Ordering::Less,
                            (false, true) => Ordering::Greater,
                        }
                    })
                    .collect::<Vec<_>>();
                let mut sock: SocketAddr = addrs_sorted.pop().unwrap();
                if sock.port() == 0 {
                    sock.set_port(SSH_PORT_DEFAULT);
                }
                Ok(ResolutionTarget::Addr(sock))
            }
            TargetState::Fastboot(fts) => Ok(ResolutionTarget::Serial(fts.serial_number.clone())),
            state => {
                Err(anyhow::anyhow!("Target discovered but not in the correct state: {state:?}"))
            }
        }
    }

    fn to_spec(&self) -> String {
        match &self {
            ResolutionTarget::Addr(ssh_addr) => {
                format!("{ssh_addr}")
            }
            ResolutionTarget::Serial(serial) => {
                format!("serial:{serial}")
            }
        }
    }
}

pub struct Resolution {
    target: ResolutionTarget,
    discovered: Option<TargetHandle>,
    pub connection: Option<Connection>,
    rcs_proxy: Option<RemoteControlProxy>,
    identify_host_response: Option<IdentifyHostResponse>,
}

impl Resolution {
    fn from_target(target: ResolutionTarget) -> Self {
        Self {
            target,
            discovered: None,
            connection: None,
            rcs_proxy: None,
            identify_host_response: None,
        }
    }
    fn from_addr(sa: SocketAddr) -> Self {
        let scope_id = if let SocketAddr::V6(addr) = sa { addr.scope_id() } else { 0 };
        let port = match sa.port() {
            0 => SSH_PORT_DEFAULT,
            p => p,
        };
        Self::from_target(ResolutionTarget::Addr(TargetAddr::new(sa.ip(), scope_id, port).into()))
    }

    fn from_target_handle(th: TargetHandle) -> Result<Self> {
        let target = ResolutionTarget::from_target_handle(&th)?;
        Ok(Self { discovered: Some(th), ..Self::from_target(target) })
    }

    pub fn addr(&self) -> Result<SocketAddr> {
        match self.target {
            ResolutionTarget::Addr(addr) => Ok(addr),
            _ => bail!("target resolved to serial, not socket_addr"),
        }
    }

    pub async fn get_connection(&mut self, context: &EnvironmentContext) -> Result<&Connection> {
        if self.connection.is_none() {
            let connector = SshConnector::new(self.addr()?, context).await?;
            let conn = Connection::new(connector)
                .await
                .map_err(|e| crate::KnockError::CriticalError(e.into()))?;
            self.connection = Some(conn);
        }
        Ok(self.connection.as_ref().unwrap())
    }

    pub async fn get_rcs_proxy(
        &mut self,
        context: &EnvironmentContext,
    ) -> Result<&RemoteControlProxy> {
        if self.rcs_proxy.is_none() {
            let conn = self.get_connection(context).await?;
            self.rcs_proxy = Some(conn.rcs_proxy().await?);
        }
        Ok(self.rcs_proxy.as_ref().unwrap())
    }

    pub async fn identify(
        &mut self,
        context: &EnvironmentContext,
    ) -> Result<&IdentifyHostResponse> {
        if self.identify_host_response.is_none() {
            let rcs_proxy = self.get_rcs_proxy(context).await?;
            self.identify_host_response = Some(
                rcs_proxy
                    .identify_host()
                    .await?
                    .map_err(|e| anyhow::anyhow!("Error identifying host: {e:?}"))?,
            );
        }
        Ok(self.identify_host_response.as_ref().unwrap())
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[fuchsia::test]
    async fn test_can_resolve_target_locally() {
        let test_env = ffx_config::test_init().await.unwrap();
        let mut resolver = MockQueryResolverT::new();
        // A network address will resolve to itself
        let addr = "127.0.0.1:123".to_string();
        let addr_spec = Some(addr.clone());
        // Note that this will fail if we try to call resolve_target_spec()
        // since we haven't mocked a return value. So it's also checking that no
        // resolution is done.
        let target_spec =
            locally_resolve_target_spec(addr_spec.clone(), &resolver, &test_env.context)
                .await
                .unwrap();
        assert_eq!(target_spec, addr_spec.clone());

        // A serial spec will resolve to itself
        let sn = "abcdef".to_string();
        let sn_spec = format!("serial:{sn}");
        // Note that this will fail if we try to call resolve_target_spec()
        // since we still haven't mocked a return value. So it's also checking that no
        // resolution is done.
        let target_spec =
            locally_resolve_target_spec(Some(sn_spec.clone()), &resolver, &test_env.context)
                .await
                .unwrap();
        assert_eq!(target_spec, Some(sn_spec.clone()));

        // A DNS name will satisfy the resolution request
        let name_spec = Some("foobar".to_string());
        let sa = addr.parse::<SocketAddr>().unwrap();
        let state = TargetState::Product(vec![sa.into()]);
        let th = TargetHandle { node_name: name_spec.clone(), state };
        resolver.expect_resolve_target_query().return_once(move |_, _| Ok(vec![th]));
        resolver.expect_try_resolve_manual_target().return_once(move |_, _| Ok(None));
        let target_spec =
            locally_resolve_target_spec(name_spec.clone(), &resolver, &test_env.context)
                .await
                .unwrap();
        assert_eq!(target_spec, addr_spec);

        // A serial number for an existing target will satisfy the resolution request
        let mut resolver = MockQueryResolverT::new();
        let th = TargetHandle {
            node_name: None,
            state: TargetState::Fastboot(discovery::FastbootTargetState {
                serial_number: sn.clone(),
                connection_state: discovery::FastbootConnectionState::Usb,
            }),
        };
        resolver.expect_resolve_target_query().return_once(move |_, _| Ok(vec![th]));
        resolver.expect_try_resolve_manual_target().return_once(move |_, _| Ok(None));
        // Test with "<serial>", _not_ "serial:<serial>"
        let target_spec =
            locally_resolve_target_spec(Some(sn.clone()), &resolver, &test_env.context)
                .await
                .unwrap();
        assert_eq!(target_spec, Some(sn_spec));

        // An ambiguous name will result in an error
        let mut resolver = MockQueryResolverT::new();
        let name_spec = Some("foobar".to_string());
        let sa = addr.parse::<SocketAddr>().unwrap();
        let ts1 = TargetState::Product(vec![sa.into(), sa.into()]);
        let ts2 = TargetState::Product(vec![sa.into(), sa.into()]);
        let th1 = TargetHandle { node_name: name_spec.clone(), state: ts1 };
        let th2 = TargetHandle { node_name: name_spec.clone(), state: ts2 };
        resolver.expect_resolve_target_query().return_once(move |_, _| Ok(vec![th1, th2]));
        resolver.expect_try_resolve_manual_target().return_once(move |_, _| Ok(None));
        let target_spec_res =
            locally_resolve_target_spec(Some("foo".to_string()), &resolver, &test_env.context)
                .await;
        assert!(target_spec_res.is_err());
        assert!(dbg!(target_spec_res.unwrap_err().to_string()).contains("multiple targets"));
    }

    #[fuchsia::test]
    async fn test_locally_resolve_manual_before_mdns() {
        use manual_targets::ManualTargets;
        let test_env = ffx_config::test_init().await.unwrap();
        let mut resolver = MockQueryResolverT::new();
        let name_spec = Some("foobar".to_string());
        // Set up to return an mDNS address;
        let mdns_addr = "[fe80::8c14:9c4e:7c7c:c57]:123".to_string();
        let sa = mdns_addr.parse::<SocketAddr>().unwrap();
        let state = TargetState::Product(vec![sa.into()]);
        let th = TargetHandle { node_name: name_spec.clone(), state };
        resolver.expect_resolve_target_query().return_once(move |_, _| Ok(vec![th]));

        let mt_addr = "127.0.0.1:123".to_string();
        let mt_sa = mt_addr.parse::<SocketAddr>().unwrap();
        let mt_config = manual_targets::Config::default();
        // Also set up a manual target with a different result
        mt_config.add(mt_addr.clone(), None).await.expect("add manual target");
        resolver
            .expect_try_resolve_manual_target()
            .return_once(move |_, _| Ok(Some(Resolution::from_addr(mt_sa))));

        // Confirm that we get the manual address back
        let name_spec = Some("foobar".to_string());
        let target_spec =
            locally_resolve_target_spec(name_spec.clone(), &resolver, &test_env.context)
                .await
                .unwrap();
        assert_eq!(target_spec, Some(mt_addr));
    }

    // XXX Creating a reasonable test for the rest of the behavior:
    // * partial matching of names
    // * timing out when no matching targets return
    // * returning early when there is an exact name match
    // requires mocking a Stream<Item=TargetEvent>, which is difficult since a
    // trait for returning such a stream can't be made since the items are not ?
    // Sized (according to the rust compiler). So these additional tests will
    // require some more work.
}