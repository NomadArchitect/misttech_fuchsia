// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Error;
use cm_rust::FidlIntoNative;
use fidl::endpoints::ControlHandle;
use fidl_fuchsia_media::ProfileProviderMarker;
use fidl_fuchsia_ui_composition::{AllocatorMarker, FlatlandDisplayMarker, FlatlandMarker};
use fuchsia_component::server::ServiceFs;
use fuchsia_component_test::{
    Capability, ChildOptions, RealmBuilder, RealmBuilderParams, RealmInstance, Ref, Route,
};
use futures::{StreamExt, TryStreamExt};
use {
    cm_rust, fidl_fuchsia_ui_test_context as ui_test_context, fuchsia_async as fasync,
    fuchsia_zircon_status as zx_status,
};

/// All FIDL services that are exposed by this component's ServiceFs.
enum Service {
    ScenicRealmFactoryServer(ui_test_context::ScenicRealmFactoryRequestStream),
}

#[fuchsia::main(logging_tags = ["scenic_realm_factory"])]
async fn main() -> Result<(), Error> {
    let mut fs = ServiceFs::new_local();
    fs.dir("svc").add_fidl_service(Service::ScenicRealmFactoryServer);
    fs.take_and_serve_directory_handle()?;
    fs.for_each_concurrent(None, |conn| async move {
        match conn {
            Service::ScenicRealmFactoryServer(stream) => run_realm_factory_server(stream).await,
        }
    })
    .await;
    Ok(())
}

async fn run_realm_factory_server(stream: ui_test_context::ScenicRealmFactoryRequestStream) {
    stream
        .try_for_each_concurrent(None, |request| async {
            tracing::debug!("received a request: {:?}", &request);
            let mut task_group = fasync::TaskGroup::new();
            match request {
                ui_test_context::ScenicRealmFactoryRequest::CreateRealm { payload, responder } => {
                    let realm_server = payload.realm_server.expect("missing realm_server");

                    // Create the test realm.
                    let realm = assemble_realm(payload.renderer, payload.display_rotation).await;

                    let request_stream = realm_server.into_stream().expect("into stream");
                    task_group.spawn(async move {
                        realm_proxy::service::serve(realm, request_stream)
                            .await
                            .expect("invalid realm proxy server");
                    });
                    responder.send(Ok(())).expect("failed to response");
                }
                ui_test_context::ScenicRealmFactoryRequest::_UnknownMethod {
                    control_handle,
                    ..
                } => {
                    tracing::warn!("realm factory receive an unknown request");
                    control_handle.shutdown_with_epitaph(zx_status::Status::NOT_SUPPORTED);
                    unimplemented!();
                }
            }

            task_group.join().await;
            Ok(())
        })
        .await
        .expect("failed to serve test realm factory request stream");
}

const SCENIC: &str = "scenic";
const SCENIC_REALM_URL: &str = "#meta/scenic_no_config.cm";
const CONFIG: &str = "config";
const CONFIG_URL: &str = "#meta/config.cm";

async fn assemble_realm(
    renderer: Option<ui_test_context::RendererType>,
    display_rotation: Option<u64>,
) -> RealmInstance {
    let builder =
        RealmBuilder::with_params(RealmBuilderParams::new().from_relative_url(SCENIC_REALM_URL))
            .await
            .expect("Failed to create RealmBuilder.");

    // Only fuchsia.media.ProfileProvider is not already included by scenic_no_config.cml.
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<ProfileProviderMarker>())
                .from(Ref::parent())
                .to(Ref::child(SCENIC)),
        )
        .await
        .expect("Failed to route fuchsia.media.ProfileProvider capabilities.");

    let config = builder
        .add_child(CONFIG, CONFIG_URL, ChildOptions::new())
        .await
        .expect("Failed to add config component.");

    builder
        .add_route(
            Route::new()
                .capability(Capability::configuration("fuchsia.scenic.DisplayComposition"))
                .capability(Capability::configuration(
                    "fuchsia.scenic.FrameSchedulerMinPredictedFrameDurationInUs",
                ))
                .capability(Capability::configuration("fuchsia.scenic.ICanHazDisplayId"))
                .capability(Capability::configuration("fuchsia.scenic.ICanHazDisplayMode"))
                .capability(Capability::configuration(
                    "fuchsia.scenic.MaxDisplayHorizontalResolutionPx",
                ))
                .capability(Capability::configuration(
                    "fuchsia.scenic.MaxDisplayRefreshRateMillihertz",
                ))
                .capability(Capability::configuration(
                    "fuchsia.scenic.MaxDisplayVerticalResolutionPx",
                ))
                .capability(Capability::configuration(
                    "fuchsia.scenic.MinDisplayHorizontalResolutionPx",
                ))
                .capability(Capability::configuration(
                    "fuchsia.scenic.MinDisplayRefreshRateMillihertz",
                ))
                .capability(Capability::configuration(
                    "fuchsia.scenic.MinDisplayVerticalResolutionPx",
                ))
                .capability(Capability::configuration("fuchsia.scenic.PointerAutoFocus"))
                .from(&config)
                .to(Ref::child(SCENIC)),
        )
        .await
        .expect("Failed to route config capabilities.");

    let renderer_type = match renderer {
        Some(ui_test_context::RendererType::Cpu) => "cpu".to_string(),
        Some(ui_test_context::RendererType::Null) => "null".to_string(),
        Some(ui_test_context::RendererType::Vulkan) => "vulkan".to_string(),
        _ => "vulkan".to_string(),
    };

    builder
        .add_capability(cm_rust::CapabilityDecl::Config(cm_rust::ConfigurationDecl {
            name: "fuchsia.scenic.Renderer".to_string().fidl_into_native(),
            value: renderer_type.into(),
        }))
        .await
        .expect("Failed to set fuchsia.scenic.Renderer.");

    let display_rotation_value: u64 = match display_rotation {
        Some(value) => value,
        None => 0,
    };

    builder
        .add_capability(cm_rust::CapabilityDecl::Config(cm_rust::ConfigurationDecl {
            name: "fuchsia.scenic.DisplayRotation".to_string().fidl_into_native(),
            value: display_rotation_value.into(),
        }))
        .await
        .expect("Failed to set fuchsia.scenic.DisplayRotation.");

    builder
        .add_route(
            Route::new()
                .capability(Capability::configuration("fuchsia.scenic.Renderer"))
                .capability(Capability::configuration("fuchsia.scenic.DisplayRotation"))
                .from(Ref::self_())
                .to(Ref::child(SCENIC)),
        )
        .await
        .expect("Failed to route config capabilities.");

    // Expose UI capabilities.
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<FlatlandMarker>())
                .capability(Capability::protocol::<FlatlandDisplayMarker>())
                .capability(Capability::protocol::<AllocatorMarker>())
                .from(Ref::child(SCENIC))
                .to(Ref::parent()),
        )
        .await
        .expect("Failed to route capabilities.");

    // Create the test realm.
    builder.build().await.expect("Failed to create test realm.")
}