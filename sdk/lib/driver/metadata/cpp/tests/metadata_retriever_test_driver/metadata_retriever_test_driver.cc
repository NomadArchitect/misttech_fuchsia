// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "sdk/lib/driver/metadata/cpp/tests/metadata_retriever_test_driver/metadata_retriever_test_driver.h"

#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/logging/cpp/structured_logger.h>

namespace fdf_metadata::test {

zx::result<> MetadataRetrieverTestDriver::Start() {
  zx_status_t status = InitControllerNode();
  if (status != ZX_OK) {
    FDF_SLOG(ERROR, "Failed to initialize controller node.",
             KV("status", zx_status_get_string(status)));
    return zx::error(status);
  }

  return zx::ok();
}

zx_status_t MetadataRetrieverTestDriver::InitControllerNode() {
  if (controller_node_.has_value()) {
    FDF_SLOG(ERROR, "Controller node already initialized.");
    return ZX_ERR_BAD_STATE;
  }

  zx::result connector = devfs_connector_.Bind(dispatcher());
  if (connector.is_error()) {
    FDF_SLOG(ERROR, "Failed to bind devfs connector.", KV("status", connector.status_string()));
    return connector.status_value();
  }

  fuchsia_driver_framework::DevfsAddArgs devfs_args{{.connector = std::move(connector.value())}};

  zx::result result = AddOwnedChild(kControllerNodeName, devfs_args);
  if (result.is_error()) {
    FDF_SLOG(ERROR, "Failed to add child.", KV("status", result.status_string()));
    return result.status_value();
  }

  controller_node_.emplace(std::move(result.value()));

  return ZX_OK;
}

void MetadataRetrieverTestDriver::Serve(
    fidl::ServerEnd<fuchsia_hardware_test::MetadataRetriever> request) {
  bindings_.AddBinding(dispatcher(), std::move(request), this, fidl::kIgnoreBindingClosure);
}

void MetadataRetrieverTestDriver::GetMetadata(GetMetadataCompleter::Sync& completer) {
  zx::result metadata = fdf_metadata::GetMetadata<fuchsia_hardware_test::Metadata>(incoming());

  if (metadata.is_error()) {
    FDF_SLOG(ERROR, "Failed to get metadata.", KV("status", metadata.status_string()));
    completer.Reply(fit::error(metadata.status_value()));
    return;
  }

  completer.Reply(fit::ok(std::move(metadata.value())));
}

}  // namespace fdf_metadata::test

FUCHSIA_DRIVER_EXPORT(fdf_metadata::test::MetadataRetrieverTestDriver);