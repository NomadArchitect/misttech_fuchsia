// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![allow(clippy::let_unit_value)]

use crate::fidl::FidlServer;
use crate::metadata::put_metadata_in_happy_state;
use crate::reboot::wait_and_reboot;
use anyhow::{anyhow, Context, Error};
use config::Config;
use fidl_fuchsia_hardware_power_statecontrol::AdminMarker as PowerStateControlMarker;
use fidl_fuchsia_paver::PaverMarker;
use fidl_fuchsia_update_verify::{BlobfsVerifierMarker, NetstackVerifierMarker};
use fuchsia_async as fasync;
use fuchsia_component::client::connect_to_protocol;
use fuchsia_component::server::ServiceFs;
use fuchsia_inspect::health::Reporter;
use fuchsia_inspect::{self as finspect};
use futures::channel::oneshot;
use futures::prelude::*;
use futures::stream::FuturesUnordered;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{error, info, warn};
use zx::{self as zx, HandleBased};

mod config;
mod fidl;
mod metadata;
mod reboot;

// The system persists the reboot reason via a component called last_reboot. If we issue a reboot
// before last_reboot starts, the reboot reason will not persist. Since last_reboot is v1 and the
// system-update-committer is v2, and v2 components start earlier than v1 components, let's try to
// minimize this risk by defining a minimum duration we must wait to issue a reboot. Ideally, reboot
// clients should not have to worry about this. However, given the transition from v1 to v2, for now
// we mitigate this with a timer. This value was determined experimentally on Astro, where there
// seems to be a ~2 second gap between the system-update-committer and last_reboot starting.
const MINIMUM_REBOOT_WAIT: Duration = Duration::from_secs(5);

#[fuchsia::main(logging_tags = ["system-update-committer"])]
pub fn main() -> Result<(), Error> {
    info!("starting system-update-committer");

    let mut executor = fasync::LocalExecutor::new();
    let () = executor.run_singlethreaded(main_inner_async()).map_err(|err| {
        // Use anyhow to print the error chain.
        let err = anyhow!(err);
        error!("error running system-update-committer: {:#}", err);
        err
    })?;

    info!("shutting down system-update-committer");
    Ok(())
}

async fn main_inner_async() -> Result<(), Error> {
    let inspector = finspect::Inspector::default();
    let _inspect_server_task =
        inspect_runtime::publish(&inspector, inspect_runtime::PublishOptions::default());

    let verification_node = inspector.root().create_child("verification");
    let commit_node = metadata::CommitInspect::new(inspector.root().create_child("commit"));
    let mut health_node = finspect::health::Node::new(inspector.root());

    let verification_node_ref = &verification_node;
    let commit_node_ref = &commit_node;
    let health_node_ref = &mut health_node;

    let config = Config::load_from_config_data_or_default();

    let reboot_deadline = Instant::now() + MINIMUM_REBOOT_WAIT;

    let paver = connect_to_protocol::<PaverMarker>().context("while connecting to paver")?;
    let (boot_manager, boot_manager_server_end) = ::fidl::endpoints::create_proxy();

    paver
        .find_boot_manager(boot_manager_server_end)
        .context("transport error while calling find_boot_manager()")?;

    let reboot_proxy = connect_to_protocol::<PowerStateControlMarker>()
        .context("while connecting to power state control")?;
    let blobfs_verifier = connect_to_protocol::<BlobfsVerifierMarker>()
        .context("while connecting to blobfs verifier")?;
    let netstack_verifier = connect_to_protocol::<NetstackVerifierMarker>()
        .context("while connecting to netstack verifier")?;

    let futures = FuturesUnordered::new();
    let (p_internal, p_external) = zx::EventPair::create();

    // Keep a copy of the internal pair so that external consumers don't observe EVENTPAIR_PEER_CLOSED.
    let _p_internal_clone =
        p_internal.duplicate_handle(zx::Rights::NONE).context("while duplicating p_internal")?;

    let (unblocker, blocker) = oneshot::channel();

    // Handle putting boot metadata in happy state, rebooting on failure (if necessary), and
    // reporting health to the inspect health node.
    futures.push(
        async move {
            if let Err(e) = put_metadata_in_happy_state(
                &boot_manager,
                &p_internal,
                unblocker,
                &[&blobfs_verifier, &netstack_verifier],
                verification_node_ref,
                commit_node_ref,
                &config,
            )
            .await
            {
                let msg = format!(
                    "Failed to put metadata in happy state. Rebooting at {:?} given error {:#} and {:?}",
                    reboot_deadline,
                    anyhow!(e),
                    config
                );
                health_node_ref.set_unhealthy(&msg);
                warn!("{}", msg);
                wait_and_reboot(fasync::Timer::new(reboot_deadline), &reboot_proxy).await;
            } else {
                info!("metadata is in happy state!");
                health_node_ref.set_ok();
            }
        }
        .boxed_local(),
    );

    // Handle ServiceFs and inspect
    let mut fs = ServiceFs::new_local();
    fs.take_and_serve_directory_handle().context("while serving directory handle")?;

    // Handle FIDL.
    let fidl = Arc::new(FidlServer::new(p_external, blocker));
    futures.push(FidlServer::run(fidl, fs).boxed_local());

    let () = futures.collect::<()>().await;

    Ok(())
}
