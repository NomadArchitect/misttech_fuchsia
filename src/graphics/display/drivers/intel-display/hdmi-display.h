// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_INTEL_DISPLAY_HDMI_DISPLAY_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_INTEL_DISPLAY_HDMI_DISPLAY_H_

#include <lib/mmio/mmio-buffer.h>
#include <threads.h>
#include <zircon/compiler.h>
#include <zircon/types.h>

#include <cstddef>
#include <cstdint>

#include "src/graphics/display/drivers/intel-display/ddi-physical-layer-manager.h"
#include "src/graphics/display/drivers/intel-display/display-device.h"
#include "src/graphics/display/drivers/intel-display/dpll.h"
#include "src/graphics/display/drivers/intel-display/hardware-common.h"
#include "src/graphics/display/lib/api-types-cpp/display-id.h"
#include "src/graphics/display/lib/api-types-cpp/display-timing.h"

namespace intel_display {

class HdmiDisplay : public DisplayDevice {
 public:
  HdmiDisplay(Controller* controller, display::DisplayId id, DdiId ddi_id,
              DdiReference ddi_reference, const ddk::I2cImplProtocolClient& i2c);

  HdmiDisplay(const HdmiDisplay&) = delete;
  HdmiDisplay(HdmiDisplay&&) = delete;
  HdmiDisplay& operator=(const HdmiDisplay&) = delete;
  HdmiDisplay& operator=(HdmiDisplay&&) = delete;

  ~HdmiDisplay() override;

 private:
  bool InitDdi() final;
  bool Query() final;
  bool DdiModeset(const display::DisplayTiming& mode) final;
  bool PipeConfigPreamble(const display::DisplayTiming& mode, PipeId pipe_id,
                          TranscoderId transcoder_id) final;
  bool PipeConfigEpilogue(const display::DisplayTiming& mode, PipeId pipe_id,
                          TranscoderId transcoder_id) final;
  DdiPllConfig ComputeDdiPllConfig(int32_t pixel_clock_khz) final;
  // Hdmi doesn't need the clock rate when changing the transcoder
  int32_t LoadPixelRateForTranscoderKhz(TranscoderId transcoder_id) final { return 0; }

  bool CheckPixelRate(int64_t pixel_rate_hz) final;

  ddk::I2cImplProtocolClient i2c() final { return i2c_; }

  const ddk::I2cImplProtocolClient i2c_;
};

}  // namespace intel_display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_INTEL_DISPLAY_HDMI_DISPLAY_H_