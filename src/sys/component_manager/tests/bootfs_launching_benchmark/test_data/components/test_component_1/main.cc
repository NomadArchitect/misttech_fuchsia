// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fidl.examples.routing.echo/cpp/wire.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <unistd.h>

#include "src/sys/component_manager/tests/bootfs_launching_benchmark/test_data/dylibs/test_library_1/lib.h"
#include "src/sys/component_manager/tests/bootfs_launching_benchmark/test_data/dylibs/test_library_2/lib.h"
#include "src/sys/component_manager/tests/bootfs_launching_benchmark/test_data/dylibs/test_library_3/lib.h"
#include "src/sys/component_manager/tests/bootfs_launching_benchmark/test_data/dylibs/test_library_4/lib.h"
#include "src/sys/component_manager/tests/bootfs_launching_benchmark/test_data/dylibs/test_library_5/lib.h"

template <typename T1, typename T2>
void AssertEq(const T1& a, const T2& b) {
  if (a != b) {
    abort();
  }
}

int main() {
  fidl::ClientEnd client_end = component::Connect<fidl_examples_routing_echo::Echo>().value();
  fidl::WireResult result = fidl::WireCall(client_end)->EchoString("1");
  AssertEq(result.value().response.get(), "1");
  AssertEq(test_library_1_func(), 1);
  AssertEq(test_library_2_func(), 2);
  AssertEq(test_library_3_func(), 3);
  AssertEq(test_library_4_func(), 4);
  AssertEq(test_library_5_func(), 5);
  return EXIT_SUCCESS;
}