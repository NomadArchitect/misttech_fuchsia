// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BLOCK_DRIVERS_UFS_REQUEST_PROCESSOR_H_
#define SRC_DEVICES_BLOCK_DRIVERS_UFS_REQUEST_PROCESSOR_H_

#include <fuchsia/hardware/block/driver/cpp/banjo.h>
#include <lib/mmio/mmio.h>
#include <lib/zircon-internal/thread_annotations.h>

#include <ddktl/device.h>

#include "request_list.h"

namespace ufs {

constexpr zx::duration kCommandTimeout = zx::sec(10);

class Ufs;

class RequestProcessor {
 public:
  explicit RequestProcessor(RequestList request_list, Ufs &ufs, zx::unowned_bti bti,
                            const fdf::MmioView mmio, uint32_t slot_count)
      : request_list_(std::move(request_list)),
        controller_(ufs),
        register_(mmio),
        bti_(std::move(bti)) {}
  virtual ~RequestProcessor() = default;

  template <typename Processor, typename Descriptor>
  static zx::result<std::unique_ptr<Processor>> Create(Ufs &ufs, zx::unowned_bti bti,
                                                       const fdf::MmioView mmio,
                                                       uint8_t entry_count);

  // Write the address of the list to the list base address register and set the run-stop register.
  virtual zx::result<> Init() = 0;

  // Check all slots to process completed requests. This function returns the number of completed
  // requests. This function is called by the ISR.
  virtual uint32_t IoRequestCompletion() = 0;

  RequestList &GetRequestList() { return request_list_; }

  // For testing
  void SetTimeout(zx::duration timeout) { timeout_ = timeout; }
  zx::duration GetTimeout() const { return timeout_; }

 protected:
  zx::unowned_bti &GetBti() { return bti_; }

  // Get the number of the free slot and mark it as |SlotState::kReserved|.
  zx::result<uint8_t> ReserveSlot();
  zx::result<> ClearSlot(RequestSlot &request_slot);

  // Ring the door bell.
  zx::result<> RingRequestDoorbell(uint8_t slot_num);

  // |request_list| is not thread safe.
  // A slot in |request_list| should only be accessed by one thread at a time.
  // Currently, the main thread(InitDeviceInterface()) and the I/O threads are accessing
  // |request_list_| at the same time. To solve this problem, we changed the admin commands to use a
  // dedicated slot in |request_list_| using the ReserveAdminSlot() function. Therefore, the main
  // thread can only use the admin slot, the I/O thread cannot use the admin slot and can only use
  // the remaining slots. Therefore, the main thread and the I/O thread will never access the same
  // slot.
  RequestList request_list_;

  Ufs &controller_;
  const fdf::MmioView register_;

  zx::duration timeout_ = kCommandTimeout;

 private:
  virtual zx::result<uint8_t> GetAdminCommandSlotNumber() {
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  virtual void SetDoorBellRegister(uint8_t slot_num) = 0;

  zx::unowned_bti bti_;
};

}  // namespace ufs

#endif  // SRC_DEVICES_BLOCK_DRIVERS_UFS_REQUEST_PROCESSOR_H_
