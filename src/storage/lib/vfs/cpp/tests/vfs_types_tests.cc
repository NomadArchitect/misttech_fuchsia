// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.io/cpp/wire.h>
#include <zircon/errors.h>

#include <type_traits>
#include <utility>

#include <gtest/gtest.h>

#include "src/storage/lib/vfs/cpp/vfs_types.h"
#include "src/storage/lib/vfs/cpp/vnode.h"

namespace fs {
namespace {

namespace fio = fuchsia_io;
class DummyVnode : public Vnode {
 public:
  DummyVnode() = default;
};

#define EXPECT_RESULT_OK(expr) EXPECT_TRUE((expr).is_ok())
#define EXPECT_RESULT_ERROR(error_val, expr) \
  EXPECT_TRUE((expr).is_error());            \
  EXPECT_EQ(error_val, (expr).status_value())

TEST(VnodeConnectionOptions, ValidateOptionsForDirectory) {
  class TestDirectory : public DummyVnode {
   public:
    fio::NodeProtocolKinds GetProtocols() const final { return fio::NodeProtocolKinds::kDirectory; }
  };

  TestDirectory vnode;
  EXPECT_RESULT_OK(
      vnode.ValidateOptions(*VnodeConnectionOptions::FromOpen1Flags(fio::OpenFlags::kDirectory)));
  EXPECT_RESULT_ERROR(ZX_ERR_NOT_FILE,
                      vnode.ValidateOptions(
                          *VnodeConnectionOptions::FromOpen1Flags(fio::OpenFlags::kNotDirectory)));
}

TEST(VnodeConnectionOptions, ValidateOptionsForService) {
  class TestConnector : public DummyVnode {
   public:
    fio::NodeProtocolKinds GetProtocols() const final { return fio::NodeProtocolKinds::kConnector; }
  };

  TestConnector vnode;
  EXPECT_RESULT_ERROR(
      ZX_ERR_NOT_DIR,
      vnode.ValidateOptions(*VnodeConnectionOptions::FromOpen1Flags(fio::OpenFlags::kDirectory)));
  EXPECT_RESULT_OK(vnode.ValidateOptions(
      *VnodeConnectionOptions::FromOpen1Flags(fio::OpenFlags::kNotDirectory)));
}

TEST(VnodeConnectionOptions, ValidateOptionsForFile) {
  class TestFile : public DummyVnode {
   public:
    fio::NodeProtocolKinds GetProtocols() const final { return fio::NodeProtocolKinds::kFile; }
  };

  TestFile vnode;
  EXPECT_RESULT_ERROR(
      ZX_ERR_NOT_DIR,
      vnode.ValidateOptions(*VnodeConnectionOptions::FromOpen1Flags(fio::OpenFlags::kDirectory)));
  EXPECT_RESULT_OK(vnode.ValidateOptions(
      *VnodeConnectionOptions::FromOpen1Flags(fio::OpenFlags::kNotDirectory)));
}

}  // namespace

}  // namespace fs
