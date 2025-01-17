// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <iostream>

// In real code, use the macros in <zircon/availability.h> for such checks.
// This is an exceptional case where a check for equality is needed.
#if __Fuchsia_API_level__ == 17
#include "lib/hello_printer_17.h"
#endif

int main() {
#if __Fuchsia_API_level__ == 17
  hello_printer_17::HelloPrinter printer;
  printer.PrintHello();
#endif
  std::cout << "Hello, my dear in-tree Bazel world!\n";
  return 0;
}
