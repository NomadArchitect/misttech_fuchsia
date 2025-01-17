// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#ifndef SRC_DEVICES_BOARD_DRIVERS_QEMU_ARM64_QEMU_PCIROOT_H_
#define SRC_DEVICES_BOARD_DRIVERS_QEMU_ARM64_QEMU_PCIROOT_H_
#include <fuchsia/hardware/pciroot/cpp/banjo.h>
#include <lib/pci/pciroot.h>
#include <lib/zx/bti.h>
#include <zircon/status.h>

#include <ddktl/device.h>

namespace board_qemu_arm64 {
class QemuArm64Pciroot;
using QemuArm64PcirootType = ddk::Device<QemuArm64Pciroot, ddk::GetProtocolable>;
class QemuArm64Pciroot : public QemuArm64PcirootType,
                         public PcirootBase,
                         public ddk::PcirootProtocol<QemuArm64Pciroot> {
 public:
  struct Context {
    pci_platform_info_t info;
  };
  ~QemuArm64Pciroot() override = default;
  static zx_status_t Create(PciRootHost* root_host, QemuArm64Pciroot::Context ctx,
                            zx_device_t* parent, const char* name);
  using PcirootBase::PcirootAllocateMsi;
  using PcirootBase::PcirootDriverShouldProxyConfig;
  using PcirootBase::PcirootGetAddressSpace;
  using PcirootBase::PcirootReadConfig16;
  using PcirootBase::PcirootReadConfig32;
  using PcirootBase::PcirootReadConfig8;
  using PcirootBase::PcirootWriteConfig16;
  using PcirootBase::PcirootWriteConfig32;
  using PcirootBase::PcirootWriteConfig8;
  zx_status_t PcirootGetBti(uint32_t bdf, uint32_t index, zx::bti* bti);
  zx_status_t PcirootGetPciPlatformInfo(pci_platform_info_t* info);

  // DDK implementations
  void DdkRelease() { delete this; }
  zx_status_t DdkGetProtocol(uint32_t proto_id, void* out) {
    if (proto_id != ZX_PROTOCOL_PCIROOT) {
      return ZX_ERR_NOT_SUPPORTED;
    }

    auto* proto = static_cast<ddk::AnyProtocol*>(out);
    proto->ops = &pciroot_protocol_ops_;
    proto->ctx = this;
    return ZX_OK;
  }

 private:
  Context context_;
  QemuArm64Pciroot(PciRootHost* root_host, QemuArm64Pciroot::Context ctx, zx_device_t* parent,
                   const char* name)
      : QemuArm64PcirootType(parent), PcirootBase(root_host), context_(ctx) {}
};

}  // namespace board_qemu_arm64

#endif  // SRC_DEVICES_BOARD_DRIVERS_QEMU_ARM64_QEMU_PCIROOT_H_
