// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/blobfs/directory.h"

#include <fidl/fuchsia.io/cpp/common_types.h>
#include <lib/fdio/vfs.h>
#include <lib/zx/result.h>
#include <stdlib.h>
#include <string.h>
#include <zircon/assert.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <cassert>
#include <string_view>
#include <utility>

#include <fbl/ref_ptr.h>

#include "src/storage/blobfs/blob.h"
#include "src/storage/blobfs/blobfs.h"
#include "src/storage/blobfs/cache_node.h"
#include "src/storage/blobfs/delivery_blob.h"
#include "src/storage/blobfs/format.h"
#include "src/storage/lib/trace/trace.h"
#include "src/storage/lib/vfs/cpp/vfs.h"
#include "src/storage/lib/vfs/cpp/vfs_types.h"
#include "src/storage/lib/vfs/cpp/vnode.h"

namespace blobfs {

Directory::Directory(Blobfs* bs) : blobfs_(bs) {}

Directory::~Directory() = default;

fuchsia_io::NodeProtocolKinds Directory::GetProtocols() const {
  return fuchsia_io::NodeProtocolKinds::kDirectory;
}

zx_status_t Directory::Readdir(fs::VdirCookie* cookie, void* dirents, size_t len,
                               size_t* out_actual) {
  return blobfs_->Readdir(cookie, dirents, len, out_actual);
}

zx_status_t Directory::Read(void* data, size_t len, size_t off, size_t* out_actual) {
  return ZX_ERR_NOT_FILE;
}

zx_status_t Directory::Write(const void* data, size_t len, size_t offset, size_t* out_actual) {
  return ZX_ERR_NOT_FILE;
}

zx_status_t Directory::Append(const void* data, size_t len, size_t* out_end, size_t* out_actual) {
  return ZX_ERR_NOT_FILE;
}

zx_status_t Directory::Lookup(std::string_view name, fbl::RefPtr<fs::Vnode>* out) {
  TRACE_DURATION("blobfs", "Directory::Lookup", "name", name);
  assert(memchr(name.data(), '/', name.length()) == nullptr);

  return blobfs_->node_operations().lookup.Track([&] {
    if (name == ".") {
      // Special case: Accessing root directory via '.'
      *out = fbl::RefPtr<Directory>(this);
      return ZX_OK;
    }

    // Special case: If this is a delivery blob, we have to strip the prefix.
    if (name.length() > kDeliveryBlobPrefix.length() &&
        name.substr(0, kDeliveryBlobPrefix.length()) == kDeliveryBlobPrefix) {
      name.remove_prefix(kDeliveryBlobPrefix.length());
    }

    Digest digest;
    if (zx_status_t status = digest.Parse(name.data(), name.length()); status != ZX_OK) {
      return status;
    }
    fbl::RefPtr<CacheNode> cache_node;
    if (zx_status_t status = blobfs_->GetCache().Lookup(digest, &cache_node); status != ZX_OK) {
      return status;
    }
    auto vnode = fbl::RefPtr<Blob>::Downcast(std::move(cache_node));
    blobfs_->GetMetrics()->UpdateLookup(vnode->FileSize());
    *out = std::move(vnode);
    return ZX_OK;
  });
}

zx::result<fs::VnodeAttributes> Directory::GetAttributes() const {
  return zx::ok(fs::VnodeAttributes{
      .mode = V_TYPE_DIR | V_IRUSR,
  });
}

zx::result<fbl::RefPtr<fs::Vnode>> Directory::Create(std::string_view name, fs::CreationType type) {
  TRACE_DURATION("blobfs", "Directory::Create", "name", name);
  ZX_DEBUG_ASSERT(name.find('/') == std::string_view::npos);

  return blobfs_->node_operations().create.Track([&]() -> zx::result<fbl::RefPtr<fs::Vnode>> {
    if (type != fs::CreationType::kFile) {
      return zx::error(ZX_ERR_INVALID_ARGS);
    }

    bool is_delivery_blob = false;
    // Special case: If this is a delivery blob, we have to strip the prefix.
    if (name.length() > kDeliveryBlobPrefix.length() &&
        name.substr(0, kDeliveryBlobPrefix.length()) == kDeliveryBlobPrefix) {
      name.remove_prefix(kDeliveryBlobPrefix.length());
      is_delivery_blob = true;
    }

    Digest digest;
    if (zx_status_t status = digest.Parse(name.data(), name.length()); status != ZX_OK) {
      return zx::error(status);
    }

    fbl::RefPtr new_blob = fbl::AdoptRef(new Blob(*blobfs_, digest, is_delivery_blob));
    if (zx_status_t status = blobfs_->GetCache().Add(new_blob); status != ZX_OK) {
      return zx::error(status);
    }
    if (zx_status_t status = new_blob->Open(nullptr); status != ZX_OK) {
      return zx::error(status);
    }
    return zx::ok(std::move(new_blob));
  });
}

zx_status_t Directory::Unlink(std::string_view name, bool must_be_dir) {
  TRACE_DURATION("blobfs", "Directory::Unlink", "name", name, "must_be_dir", must_be_dir);
  assert(memchr(name.data(), '/', name.length()) == nullptr);

  return blobfs_->node_operations().unlink.Track([&] {
    Digest digest;
    if (zx_status_t status = digest.Parse(name.data(), name.length()); status != ZX_OK) {
      return status;
    }
    fbl::RefPtr<CacheNode> cache_node;
    if (zx_status_t status = blobfs_->GetCache().Lookup(digest, &cache_node); status != ZX_OK) {
      return status;
    }
    auto vnode = fbl::RefPtr<Blob>::Downcast(std::move(cache_node));
    blobfs_->GetMetrics()->UpdateLookup(vnode->FileSize());
    return vnode->QueueUnlink();
  });
}

void Directory::Sync(SyncCallback closure) {
  auto event = blobfs_->node_operations().sync.NewEvent();
  blobfs_->Sync(
      [this, cb = std::move(closure), event = std::move(event)](zx_status_t status) mutable {
        // This callback will be issued on the journal thread in the normal case. This is important
        // because the flush must happen there or it will block the main thread which would block
        // processing other requests.
        //
        // If called during shutdown this may get issued on the main thread but then the flush
        // transaction should be a no-op.
        if (status == ZX_OK) {
          status = blobfs_->Flush();
        }
        cb(status);
        event.SetStatus(status);
      });
}

}  // namespace blobfs
