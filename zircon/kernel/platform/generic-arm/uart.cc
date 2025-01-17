// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/zbi-format/driver-config.h>

#include <cstdint>

#include <arch/arm64/periphmap.h>
#include <arch/defines.h>
#include <platform/uart.h>

ktl::optional<uint32_t> PlatformUartGetIrqNumber(uint32_t irq_num) { return irq_num; }

volatile void* PlatformUartMapMmio(paddr_t paddr, size_t size) {
  return reinterpret_cast<volatile void*>(periph_paddr_to_vaddr(paddr));
}
