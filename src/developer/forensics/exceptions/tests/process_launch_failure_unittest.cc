// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be found in the LICENSE file.

#include <lib/zx/exception.h>

#include <gtest/gtest.h>

#include "src/developer/forensics/exceptions/handler_manager.h"
#include "src/developer/forensics/testing/unit_test_fixture.h"

namespace forensics {
namespace exceptions {
namespace {

using ProcessLaunchFailureTest = UnitTestFixture;

// The sandbox this test runs in is not permitted to launch processes so the handler subprocess will
// not be spawned. When this happens Handle should complete without issue, not loop forever.
//
// This is tested because we experienced an error where exceptions.cml could not successfully launch
// subprocesses and ended up handling the same exception in an unterminated loop. For more
// information, see https://fxbug.dev/42137293.
TEST_F(ProcessLaunchFailureTest, HandleOnlyOnce) {
  // TODO(https://fxbug.dev/333110044): Test with suspend enabled + disabled.
  HandlerManager handler_manager(dispatcher(), CrashCounter(&InspectRoot()), 1u,
                                 zx::duration::infinite(), /*suspend_enabled=*/false);
  handler_manager.Handle(zx::exception{});
}

}  // namespace
}  // namespace exceptions
}  // namespace forensics
